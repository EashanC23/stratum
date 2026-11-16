//! Live presenter: real desktop windows, one per widget.
//!
//! This is the direct equivalent of sketchybar's window + surface + layer stack
//! (`src/window.c:51`, `src/surface.c:26`, `src/layer.m:13`), but built on the
//! public AppKit path first:
//!
//!   borderless NSWindow (clear, shadowless, level above the menu bar)
//!     └── layer-backed NSView
//!           └── CALayer.contents = CGImage built from the widget's pixmap
//!
//! Why public API: it ships, it survives OS updates, it needs no dlsym on
//! SkyLight. What it gives up versus the original is the private-API tricks —
//! sticky Spaces (`SLSSpaceCreate`), the non-activating window tag, and
//! per-widget CGS surfaces. Those are the documented next step (HANDOFF §6a);
//! the shape of this module is deliberately the same so swapping the backend is
//! a change here and nowhere else.
//!
//! Threading: AppKit is main-thread only, so a live presenter must be created
//! and ticked from the main thread. `MacPresenter::new` asserts that with
//! `MainThreadMarker`.

#![cfg(target_os = "macos")]

use crate::model::Frame;
use crate::present::Presenter;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent, NSScreen,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowLevel, NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize};
use objc2_quartz_core::{CALayer, CATransaction, kCAGravityResize};
use std::collections::HashMap;
use std::ffi::c_void;
use tiny_skia::Pixmap;

// ---------------------------------------------------------------- CoreGraphics
// Three calls, hand-declared: CGImage from a premultiplied RGBA buffer. Using
// raw FFI keeps the crate list small and the layout explicit.

type CGImageRef = *mut c_void;
type CGDataProviderRef = *mut c_void;
type CGColorSpaceRef = *mut c_void;

/// `kCGImageAlphaPremultipliedLast` with the default byte order: CG then reads
/// the bytes in memory order, which is R,G,B,A — exactly what tiny-skia writes.
///
/// Do not "improve" this to `| kCGBitmapByteOrder32Little` (0x2001) because it
/// sounds more explicit: that combination makes CG read the buffer as
/// A,B,G,R and every widget comes out red-on-white. Verified empirically in
/// `matrix::write_info_matrix`, which writes one PNG per candidate.
const BITMAP_INFO_RGBA_PREMULTIPLIED: u32 = 1;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
    fn CGDataProviderCreateWithData(
        info: *mut c_void,
        data: *const c_void,
        size: usize,
        release: Option<unsafe extern "C" fn(*mut c_void, *const c_void, usize)>,
    ) -> CGDataProviderRef;
    fn CGDataProviderRelease(provider: CGDataProviderRef);
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
        provider: CGDataProviderRef,
        decode: *const f32,
        should_interpolate: bool,
        intent: u32,
    ) -> CGImageRef;
    fn CGImageRelease(image: CGImageRef);
}

// Only the pixel round-trip test draws a CGImage back into a bitmap context.
#[cfg(test)]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    fn CGContextDrawImage(context: CGContextRef, rect: NSRect, image: CGImageRef);
    fn CGContextRelease(context: CGContextRef);
}

#[cfg(test)]
type CGContextRef = *mut c_void;

// ImageIO, debug only: write a CGImage straight to a PNG so the serializer can
// be inspected without going anywhere near AppKit.
#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageDestinationCreateWithURL(
        url: *const c_void,
        ty: *const c_void,
        count: usize,
        options: *const c_void,
    ) -> *mut c_void;
    fn CGImageDestinationAddImage(dest: *mut c_void, image: CGImageRef, properties: *const c_void);
    fn CGImageDestinationFinalize(dest: *mut c_void) -> bool;
}

/// Debug helper: write a CGImage to `path` via ImageIO.
///
/// # Safety
///
/// `image` must be a live `CGImageRef` (from `CGImageCreate` or
/// `CGBitmapContextCreateImage`) that the caller still owns; this function does
/// not retain or release it.
pub unsafe fn write_cg_image_png(image: CGImageRef, path: &std::path::Path) -> Result<(), String> {
    use objc2_foundation::{NSString, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let ty = NSString::from_str("public.png");
    // SAFETY: both NSStrings are live locals retained for the whole call, and the
    // destination retains what it needs; no pointer outlives this block.
    unsafe {
        let dest = CGImageDestinationCreateWithURL(
            Retained::as_ptr(&url) as *const c_void,
            Retained::as_ptr(&ty) as *const c_void,
            1,
            std::ptr::null(),
        );
        if dest.is_null() {
            return Err("imageio: no destination".into());
        }
        CGImageDestinationAddImage(dest, image, std::ptr::null());
        let ok = CGImageDestinationFinalize(dest);
        if !ok {
            return Err("imageio: finalize failed".into());
        }
    }
    Ok(())
}

unsafe extern "C" fn release_buffer(info: *mut c_void, _data: *const c_void, _size: usize) {
    if !info.is_null() {
        // `info` is the Box<Vec<u8>> owner; `data` is its buffer, freed with it.
        // SAFETY: the provider calls this exactly once with the `info` we gave
        // it, which was a `Box::into_raw`'d Vec and is not used afterwards.
        drop(unsafe { Box::from_raw(info as *mut Vec<u8>) });
    }
}

/// Build a CGImage that owns a copy of the pixmap's bytes.
///
/// The copy is deliberate: `Pixmap` is reused for the next frame, and the
/// compositor may hold `layer.contents` longer than the caller's borrow.
fn cg_image_from_pixmap(pm: &Pixmap) -> Result<CGImageRef, String> {
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    if w == 0 || h == 0 {
        return Err("cg image: empty pixmap".into());
    }
    let bytes: Vec<u8> = pm.data().to_vec();
    let mut boxed = Box::new(bytes);
    // The provider wants the PIXEL BUFFER, not the Vec's struct header — passing
    // `Box::into_raw(boxed)` as the data pointer hands CG three usize fields to
    // interpret as pixels (garbage that looks like noise).
    let len = boxed.len();
    let ptr = boxed.as_mut_ptr() as *const c_void;
    let owner = Box::into_raw(boxed);

    // SAFETY: `owner` is a Box we just leaked and `ptr` is its buffer, which the
    // data provider takes ownership of and hands back to `release_buffer` exactly
    // once. The provider outlives every use because `CGImageCreate` retains it.
    unsafe {
        let space = CGColorSpaceCreateDeviceRGB();
        if space.is_null() {
            drop(Box::from_raw(owner));
            return Err("cg image: no colour space".into());
        }
        let provider =
            CGDataProviderCreateWithData(owner as *mut c_void, ptr, len, Some(release_buffer));
        if provider.is_null() {
            CGColorSpaceRelease(space);
            return Err("cg image: no data provider".into());
        }
        let image = CGImageCreate(
            w,
            h,
            8,
            32,
            w * 4,
            space,
            BITMAP_INFO_RGBA_PREMULTIPLIED,
            provider,
            std::ptr::null(),
            false,
            0,
        );
        CGDataProviderRelease(provider);
        CGColorSpaceRelease(space);
        if image.is_null() {
            return Err("cg image: CGImageCreate failed".into());
        }
        Ok(image)
    }
}

// ------------------------------------------------------------------- window ----

struct WidgetWindow {
    window: Retained<NSWindow>,
    view: Retained<MouseView>,
    layer: Retained<CALayer>,
    frame: Frame,
    /// The CGImage currently retained by the layer; released on replace.
    contents: CGImageRef,
}

/// `kCGDesktopWindowLevel` — above the wallpaper, *below* the desktop icons.
pub const DESKTOP_LEVEL: NSWindowLevel = -2147483623;
/// `kCGDesktopIconWindowLevel` — above the desktop icons, still below every app
/// window. This is the default for widgets: it reads as part of the desktop.
pub const DESKTOP_ICON_LEVEL: NSWindowLevel = -2147483603;
/// `NSStatusWindowLevel` — above the menu bar. Opt-in per widget.
pub const STATUS_LEVEL: NSWindowLevel = 25;

pub struct MacPresenter {
    mtm: MainThreadMarker,
    windows: HashMap<String, WidgetWindow>,
    /// Displays in the model's shared top-left space (index = `display`).
    screens: Vec<Frame>,
    /// Height of the main display — the model space is anchored at its top-left,
    /// so this is the only number needed to convert back to AppKit's y-up space.
    main_height: f64,
    screen: Frame,
    scale: f32,
    /// Level for widgets that opted into `always_on_top = true`.
    top_level: NSWindowLevel,
    /// Per-widget overrides, keyed by widget id.
    click_through: HashMap<String, bool>,
    always_on_top: HashMap<String, bool>,
    presented: u64,
}

impl MacPresenter {
    /// `top_level` is the level used by widgets with `always_on_top = true`
    /// (25 = above the menu bar). Everything else is placed at desktop level so
    /// app windows cover it.
    pub fn new(screens: Vec<Frame>, scale: f32, top_level: NSWindowLevel) -> Result<Self, String> {
        let mtm =
            MainThreadMarker::new().ok_or("live presenter must be created on the main thread")?;
        let app = NSApplication::sharedApplication(mtm);
        // Accessory: no Dock icon, and the process never becomes the active app,
        // so creating windows does not steal focus from what you are doing.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

        let mut screens = screens;
        if screens.is_empty() {
            screens = vec![Frame::new(0.0, 0.0, 1440.0, 900.0)];
        }
        // The model space is anchored at the main display's top-left corner, so
        // its height is the only constant needed to convert to AppKit coordinates.
        let main_height = screens
            .iter()
            .find(|f| f.x == 0.0 && f.y == 0.0)
            .map(|f| f.h as f64)
            .unwrap_or(screens[0].h as f64);
        let screen = screens[0];

        Ok(MacPresenter {
            mtm,
            windows: HashMap::new(),
            screens,
            main_height,
            screen,
            scale,
            top_level,
            click_through: HashMap::new(),
            always_on_top: HashMap::new(),
            presented: 0,
        })
    }

    /// Choose the window level for a widget: desktop by default, `top_level`
    /// only when the widget opts in. Windows are created lazily, so this must be
    /// called before the first `present`.
    pub fn set_always_on_top(&mut self, id: &str, on: bool) {
        self.always_on_top.insert(id.to_string(), on);
    }

    pub fn level_for(&self, id: &str) -> NSWindowLevel {
        if self.always_on_top.get(id).copied().unwrap_or(false) {
            self.top_level
        } else {
            DESKTOP_ICON_LEVEL
        }
    }

    pub fn set_click_through(&mut self, id: &str, on: bool) {
        self.click_through.insert(id.to_string(), on);
        if let Some(w) = self.windows.get(id) {
            w.window.setIgnoresMouseEvents(on);
        }
    }

    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    pub fn presented(&self) -> u64 {
        self.presented
    }

    /// AppKit coordinates are bottom-left, measured from the *main* display's
    /// bottom-left; the model is top-left, measured from the same display's
    /// top-left. One subtraction converts between them, for every display.
    fn appkit_rect(&self, frame: Frame) -> NSRect {
        NSRect {
            origin: NSPoint {
                x: frame.x as f64,
                y: self.main_height - (frame.y as f64 + frame.h as f64),
            },
            size: NSSize {
                width: frame.w as f64,
                height: frame.h as f64,
            },
        }
    }

    /// Drain the mouse queue and translate it into widget ids plus screen-space
    /// points (top-left origin, the same space as `Frame`).
    pub fn poll_mouse(&self) -> Vec<(String, MouseKind, f32, f32)> {
        let mut out = Vec::new();
        for e in take_events() {
            // Synthetic events (--inject-click) carry screen coordinates already.
            if e.window_number < 0 {
                out.push(("<injected>".to_string(), e.kind, e.x, e.y));
                continue;
            }
            let Some((id, f)) = self.windows.iter().find_map(|(id, w)| {
                (w.window.windowNumber() == e.window_number).then_some((id.clone(), w.frame))
            }) else {
                continue;
            };
            // Window-local y is up from the bottom; the model is down from the top.
            out.push((id, e.kind, f.x + e.x, f.y + (f.h - e.y)));
        }
        out
    }

    pub fn screens(&self) -> &[Frame] {
        &self.screens
    }

    /// Re-read the display layout. Called when the configuration changes (a
    /// monitor plugged in, a resolution change, waking from sleep); existing
    /// windows are left in place and moved on their next `present`.
    pub fn set_screens(&mut self, screens: Vec<Frame>) {
        if screens.is_empty() {
            return;
        }
        self.main_height = screens
            .iter()
            .find(|f| f.x == 0.0 && f.y == 0.0)
            .map(|f| f.h as f64)
            .unwrap_or(screens[0].h as f64);
        self.screen = screens[0];
        self.screens = screens;
    }

    fn create_window(&mut self, id: &str, frame: Frame, pm: &Pixmap) -> Result<(), String> {
        let rect = self.appkit_rect(frame);

        // SAFETY: NSWindow is main-thread only and `self.mtm` proves we are on
        // that thread; `init…` returns +1 and `Retained` owns it.
        let window: Retained<NSWindow> = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                self.mtm.alloc::<NSWindow>(),
                rect,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };

        // A widget is a transparent plate: no titlebar, no chrome, no shadow,
        // no background — anything opaque here would paint a rectangle where the
        // rounded corners are supposed to show through.
        // SAFETY: every AppKit call below is a plain property setter on a window this
        // function just allocated on the main thread (`mtm` is held by the presenter),
        // which is the only thread AppKit allows.
        unsafe {
            window.setOpaque(false);
            window.setBackgroundColor(Some(&NSColor::clearColor()));
            window.setHasShadow(false);
            window.setLevel(self.level_for(id));
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
            window.setIgnoresMouseEvents(self.click_through.get(id).copied().unwrap_or(true));
            window.setReleasedWhenClosed(false);
        }

        let rect0 = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize {
                width: frame.w as f64,
                height: frame.h as f64,
            },
        };
        let view: Retained<MouseView> =
        // SAFETY: `MouseView` is declared by `define_class!` with NSView as its
        // superclass, so `initWithFrame:` is valid; `alloc`/`init` returns +1 and
        // `Retained` takes ownership of exactly one reference.
            unsafe { msg_send![self.mtm.alloc::<MouseView>(), initWithFrame: rect0] };
        view.setWantsLayer(true);
        // Tracking areas with ActiveAlways deliver enter/exit even though the
        // window never becomes key — the only way hover works for a widget.
        // SAFETY: the tracking area is allocated here, owns no borrowed data (owner is
        // a live view for the area's lifetime), and the view retains it on `add:`.
        unsafe {
            let area = NSTrackingArea::initWithRect_options_owner_userInfo(
                self.mtm.alloc::<NSTrackingArea>(),
                rect0,
                NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::ActiveAlways
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(&*(Retained::as_ptr(&view) as *const objc2::runtime::AnyObject)),
                None,
            );
            view.addTrackingArea(&area);
        }
        let layer = view
            .layer()
            .ok_or_else(|| format!("widget {id}: layer-backed view has no layer"))?;

        layer.setContentsScale(self.scale as f64);
        // SAFETY: `kCAGravityResize` is a process-lifetime constant NSString
        // declared by the framework; reading it is always valid.
        layer.setContentsGravity(unsafe { kCAGravityResize });
        layer.setOpaque(false);

        let contents = cg_image_from_pixmap(pm)?;
        // SAFETY: `window` and `view` are live objects created above and `contents` is
        // a +1 CGImage we keep in `WidgetWindow` until the layer is replaced.
        unsafe {
            window.setContentView(Some(&view));
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            layer.setContents(Some(&*(contents as *const objc2::runtime::AnyObject)));
            CATransaction::commit();
        }

        window.orderFrontRegardless();

        self.windows.insert(
            id.to_string(),
            WidgetWindow {
                window,
                view,
                layer,
                frame,
                contents,
            },
        );
        Ok(())
    }

    fn update_contents(&mut self, id: &str, pm: &Pixmap) -> Result<(), String> {
        let contents = cg_image_from_pixmap(pm)?;
        let w = self
            .windows
            .get_mut(id)
            .ok_or_else(|| format!("widget {id}: window missing"))?;

        // SAFETY: same as `create_window` — the layer belongs to a window this presenter
        // owns, and `layer.contents` retains the CGImage we hand it.
        unsafe {
            CATransaction::begin();
            // Without this every contents swap animates over ~250ms and the
            // widget ghosts between values.
            CATransaction::setDisableActions(true);
            w.layer
                .setContents(Some(&*(contents as *const objc2::runtime::AnyObject)));
            CATransaction::commit();
            if !w.contents.is_null() {
                CGImageRelease(w.contents);
            }
        }
        w.contents = contents;
        Ok(())
    }

    /// Pump AppKit's run loop for `seconds` so windows actually draw and the
    /// compositor sees our flushes. Called between ticks; never blocks forever.
    pub fn pump(&self, seconds: f64) {
        let run_loop = NSRunLoop::currentRunLoop();
        let deadline = NSDate::dateWithTimeIntervalSinceNow(seconds);
        run_loop.runUntilDate(&deadline);
    }
}

/// What a widget window reports about itself, for `live --snapshot`.
///
/// Deliberately no occlusion state: `NSWindow.occlusionState` reads false for
/// every window in a short-lived Accessory process (measured — even at level 25,
/// after 1.5s of run-loop pumping), so it would be a misleading column. Stacking
/// is provable from `level` instead: anything below 0 is under normal windows.
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: String,
    pub frame: Frame,
    pub appkit_origin: (f64, f64),
    pub level: isize,
    pub visible: bool,
    pub window_number: isize,
    pub alpha: f64,
}

impl MacPresenter {
    /// Read back what AppKit thinks each window is, and capture what the layer
    /// tree actually contains.
    ///
    /// The pixel capture goes through `NSView.cacheDisplayInRect:toBitmapImageRep:`,
    /// which composites the view's layer tree in-process — it needs no Screen
    /// Recording permission, unlike `screencapture`. If these PNGs look right,
    /// the CGImage → CALayer.contents path is correct; whether the window server
    /// then shows them is a separate question answered by `visible`/`level`.
    pub fn snapshot(&self, dir: &std::path::Path) -> Result<Vec<(WindowInfo, String)>, String> {
        use objc2_app_kit::NSBitmapImageRep;
        use objc2_foundation::{NSDictionary, NSString};

        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut out = Vec::new();

        for (id, w) in &self.windows {
            let f = w.frame;
            let info = WindowInfo {
                id: id.clone(),
                frame: f,
                appkit_origin: (w.window.frame().origin.x, w.window.frame().origin.y),
                level: w.window.level(),
                visible: w.window.isVisible(),
                window_number: w.window.windowNumber(),
                alpha: w.window.alphaValue(),
            };

            let bounds = w.view.bounds();
            let path = dir.join(format!("{id}.png"));
            let mut wrote = String::from("(no bitmap rep)");
            if let Some(rep) = w.view.bitmapImageRepForCachingDisplayInRect(bounds) {
                w.view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
                let props = NSDictionary::new();
                // SAFETY: `rep` is a live NSBitmapImageRep and the returned NSData
                // is retained before this block ends.
                if let Some(data) = unsafe {
                    NSBitmapImageRep::representationUsingType_properties(
                        &rep,
                        objc2_app_kit::NSBitmapImageFileType::PNG,
                        &props,
                    )
                } {
                    let ns_path = NSString::from_str(&path.to_string_lossy());
                    let ok = data.writeToFile_atomically(&ns_path, true);
                    wrote = if ok {
                        path.display().to_string()
                    } else {
                        format!("(write failed: {})", path.display())
                    };
                }
            }
            out.push((info, wrote));
        }

        out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
        Ok(out)
    }
}

// ------------------------------------------------------------------ mouse ----

/// A mouse event, in the model's shared top-left space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseEvent {
    pub window_number: isize,
    pub kind: MouseKind,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Up,
    Entered,
    Exited,
}

/// Queue filled by the view (which runs inside AppKit's event dispatch) and
/// drained by the presenter on its next pump. A mutex around a Vec is enough:
/// there is exactly one AppKit thread and one consumer.
static QUEUE: std::sync::Mutex<Vec<MouseEvent>> = std::sync::Mutex::new(Vec::new());

fn push_event(event: MouseEvent) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push(event);
    }
}

/// Drain everything the view captured since the last call.
pub fn take_events() -> Vec<MouseEvent> {
    QUEUE
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// Inject a synthetic click, for `live --inject-click`. Lets the event path be
/// exercised end to end without a human at the mouse.
pub fn inject_click(x: f32, y: f32) {
    push_event(MouseEvent {
        window_number: -1,
        kind: MouseKind::Down,
        x,
        y,
    });
}

objc2::define_class!(
    /// A plain layer-backed view that forwards mouse events to the queue.
    ///
    /// It exists because AppKit only delivers `mouseDown:`/`mouseEntered:` to a
    /// view that implements them — a bare `NSView` receives nothing. The window
    /// is non-activating, so tracking areas use `ActiveAlways`; without that the
    /// view would only see the mouse once the app became active, which never
    /// happens for a desktop widget.
    #[unsafe(super(NSView))]
    #[name = "StratumMouseView"]
    #[ivars = ()]
    pub struct MouseView;

    impl MouseView {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            queue_from_event(event, self.window_number(), MouseKind::Down);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            queue_from_event(event, self.window_number(), MouseKind::Up);
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            queue_from_event(event, self.window_number(), MouseKind::Entered);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            queue_from_event(event, self.window_number(), MouseKind::Exited);
        }

        // A widget should respond to the first click, not just the second.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: &NSEvent) -> bool {
            true
        }
    }
);

impl MouseView {
    fn window_number(&self) -> isize {
        self.window().map(|w| w.windowNumber()).unwrap_or(-1)
    }
}

fn queue_from_event(event: &NSEvent, window_number: isize, kind: MouseKind) {
    let p = event.locationInWindow();
    // locationInWindow is y-up from the window's bottom-left; the model is y-down
    // from the display's top-left, and the presenter converts the rest.
    push_event(MouseEvent {
        window_number,
        kind,
        x: p.x as f32,
        y: p.y as f32,
    });
}

impl Presenter for MacPresenter {
    fn present(&mut self, id: &str, frame: Frame, image: &Pixmap) -> Result<(), String> {
        self.presented += 1;
        match self.windows.get(id) {
            None => self.create_window(id, frame, image)?,
            Some(w) => {
                let moved = (w.frame.x - frame.x).abs() > 0.01
                    || (w.frame.y - frame.y).abs() > 0.01
                    || (w.frame.w - frame.w).abs() > 0.01
                    || (w.frame.h - frame.h).abs() > 0.01;
                if moved {
                    let rect = self.appkit_rect(frame);
                    let win = &self.windows[id].window;
                    win.setFrame_display(rect, true);

                    if let Some(w) = self.windows.get_mut(id) {
                        w.frame = frame;
                    }
                }
                self.update_contents(id, image)?;
            }
        }
        Ok(())
    }

    fn reposition(&mut self, id: &str, frame: Frame) -> Result<(), String> {
        let rect = self.appkit_rect(frame);
        if let Some(w) = self.windows.get_mut(id)
            && w.frame != frame
        {
            w.window.setFrame_display(rect, true);
            w.frame = frame;
        }
        Ok(())
    }

    fn destroy(&mut self, id: &str) -> Result<(), String> {
        if let Some(w) = self.windows.remove(id) {
            // SAFETY: `orderOut:` on a live window, then releasing exactly the +1 CGImage
            // that `WidgetWindow::contents` holds; the field is set to null by the drop of
            // the struct right after.
            unsafe {
                w.window.orderOut(None);
                if !w.contents.is_null() {
                    CGImageRelease(w.contents);
                }
            }
        }
        Ok(())
    }
}

impl Drop for MacPresenter {
    fn drop(&mut self) {
        let ids: Vec<String> = self.windows.keys().cloned().collect();
        for id in ids {
            let _ = self.destroy(&id);
        }
    }
}

/// Every display, converted into the model's single top-left space anchored at
/// the main display's top-left corner. Displays left of / above the main one get
/// negative coordinates, which is exactly right for hit-testing and anchoring.
pub fn displays() -> Vec<Frame> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Vec::new();
    };
    let main_h = match NSScreen::mainScreen(mtm) {
        Some(s) => s.frame().size.height,
        None => return Vec::new(),
    };
    let list = NSScreen::screens(mtm);
    let mut out = Vec::new();
    for screen in list.iter() {
        let f = screen.frame();
        out.push(Frame::new(
            f.origin.x as f32,
            (main_h - (f.origin.y + f.size.height)) as f32,
            f.size.width as f32,
            f.size.height as f32,
        ));
    }
    out
}

/// Just the main display, for callers that only need a size.
pub fn main_screen_frame() -> Option<Frame> {
    displays().into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Color;

    /// The CGImage we hand to CALayer must round-trip byte-for-byte. If this
    /// passes, a wrong-looking widget on screen is an AppKit issue, not a
    /// colour-order/stride bug in our serializer.
    #[test]
    fn cg_image_roundtrips_pixels() {
        let (w, h) = (4u32, 3u32);
        let mut pm = Pixmap::new(w, h).expect("pixmap");
        // Distinct, premultiplied-safe colours: opaque, so premultiplication is
        // the identity and the comparison is exact.
        pm.fill(Color::from_rgba8(0x11, 0x22, 0x33, 0xff));
        pm.pixels_mut()[0] = tiny_skia::PremultipliedColorU8::from_rgba(0xcc, 0x44, 0x11, 0xff)
            .expect("premultiplied");

        let image = cg_image_from_pixmap(&pm).expect("cgimage");
        assert!(!image.is_null());

        // Dump it so a human (or vision) can see what CG thinks we gave it.
        let dump = std::path::Path::new("/tmp/cg_image_probe.png");
        // SAFETY: `image` was created just above and is released after this test.
        if let Err(e) = unsafe { write_cg_image_png(image, dump) } {
            eprintln!("debug dump failed: {e}");
        }

        let mut out = vec![0u8; (w * h * 4) as usize];
        // SAFETY: `out` is a live buffer sized exactly w*h*4, and the context is
        // released before `out` is read; the context does not outlive the buffer.
        unsafe {
            let space = CGColorSpaceCreateDeviceRGB();
            let ctx = CGBitmapContextCreate(
                out.as_mut_ptr() as *mut c_void,
                w as usize,
                h as usize,
                8,
                (w * 4) as usize,
                space,
                BITMAP_INFO_RGBA_PREMULTIPLIED,
            );
            assert!(!ctx.is_null(), "bitmap context");
            // CG wants a y-up rect in user space; one image-sized rect covers it.
            CGContextDrawImage(
                ctx,
                NSRect {
                    origin: NSPoint { x: 0.0, y: 0.0 },
                    size: NSSize {
                        width: w as f64,
                        height: h as f64,
                    },
                },
                image,
            );
            CGContextRelease(ctx);
            CGColorSpaceRelease(space);
            CGImageRelease(image);
        }

        // Row 0 of a CGBitmapContext's memory is the top row of the image, same
        // as the pixmap's row 0 — so the comparison is direct, no flip.
        let src = pm.data();
        for y in 0..h as usize {
            for x in 0..(w as usize) {
                let s = (y * w as usize + x) * 4;
                let d = (y * w as usize + x) * 4;
                assert_eq!(
                    &src[s..s + 4],
                    &out[d..d + 4],
                    "pixel ({x},{y}) mismatch: src {:?} dst {:?}",
                    &src[s..s + 4],
                    &out[d..d + 4]
                );
            }
        }
    }
}

#[cfg(test)]
mod matrix {
    use super::*;
    use tiny_skia::{Color, Pixmap};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGBitmapContextCreateImage(context: CGContextRef) -> CGImageRef;
    }

    /// Empirical check of which CGBitmapInfo describes tiny-skia's buffer.
    /// Writes /tmp/cg_matrix_<info>.png for each candidate.
    #[test]
    fn write_info_matrix() {
        let (w, h) = (8u32, 8u32);
        let mut pm = Pixmap::new(w, h).expect("pixmap");
        pm.fill(Color::from_rgba8(0x11, 0x22, 0x33, 0xff));
        for y in 0..4u32 {
            for x in 0..4u32 {
                pm.pixels_mut()[(y * w + x) as usize] =
                    tiny_skia::PremultipliedColorU8::from_rgba(0xcc, 0x44, 0x11, 0xff).unwrap();
            }
        }

        for info in [0x2001u32, 0x2002, 0x0001, 0x0002, 0x4001] {
            // SAFETY: as above — a caller-owned buffer, a context released before the buffer
            // is read, and an image released after being written out.
            unsafe {
                let space = CGColorSpaceCreateDeviceRGB();
                let ctx = CGBitmapContextCreate(
                    pm.data().as_ptr() as *mut c_void,
                    w as usize,
                    h as usize,
                    8,
                    (w * 4) as usize,
                    space,
                    info,
                );
                if ctx.is_null() {
                    eprintln!("info {info:#x}: context failed");
                    CGColorSpaceRelease(space);
                    continue;
                }
                let image = CGBitmapContextCreateImage(ctx);
                if image.is_null() {
                    eprintln!("info {info:#x}: image failed");
                } else {
                    let path = std::path::PathBuf::from(format!("/tmp/cg_matrix_{info:04x}.png"));
                    match write_cg_image_png(image, &path) {
                        Ok(()) => eprintln!("wrote {}", path.display()),
                        Err(e) => eprintln!("info {info:#x}: {e}"),
                    }
                    CGImageRelease(image);
                }
                CGContextRelease(ctx);
                CGColorSpaceRelease(space);
            }
        }
    }
}
