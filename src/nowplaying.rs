//! Now-playing, straight from macOS.
//!
//! No `nowplaying-cli`, no AppleScript, no extra binary: this calls
//! `MRMediaRemoteGetNowPlayingInfo` in `/System/Library/PrivateFrameworks/MediaRemote.framework`,
//! which is the same source the Now Playing widget in Control Centre uses.
//! The symbol is not in the framework's static symbol table on recent macOS, so
//! it is resolved with `dlsym` at runtime — and if a future release removes it,
//! this reports that instead of failing to launch.
//!
//! The API is asynchronous: it takes a dispatch queue and a block, and calls the
//! block on that queue with a `CFDictionaryRef` that is only valid for the
//! duration of the call. Everything is therefore extracted inside the block and
//! sent back over a channel.
//!
//! `STRATUM_NOWPLAYING_JSON` short-circuits the whole thing with a JSON object
//! (`{title, artist, album, elapsed, duration, playing}`), which is what the
//! tests and the docs screenshots use on a machine with nothing playing.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub elapsed: f64,
    pub duration: f64,
    pub playing: bool,
}

impl Track {
    /// Seconds left, clamped at zero (a paused track can report elapsed > duration).
    pub fn remaining(&self) -> f64 {
        (self.duration - self.elapsed).max(0.0)
    }

    pub fn progress_percent(&self) -> f64 {
        if self.duration > 0.0 {
            (self.elapsed / self.duration * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        }
    }

    /// `Artist — Title`, or just the title when there is no artist.
    pub fn one_line(&self) -> String {
        match (self.artist.is_empty(), self.title.is_empty()) {
            (true, true) => "nothing playing".to_string(),
            (true, false) => self.title.clone(),
            (false, true) => self.artist.clone(),
            (false, false) => format!("{} — {}", self.artist, self.title),
        }
    }
}

// ---------------------------------------------------------------- FFI ------

const RTLD_LAZY: c_int = 0x1;
const FRAMEWORK: &str = "/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote";
const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
const CF_DOUBLE: i64 = 13; // kCFNumberDoubleType

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFNumberGetValue(n: *const c_void, kind: i64, out: *mut c_void) -> bool;
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFBooleanGetValue(b: *const c_void) -> bool;
    fn CFBooleanGetTypeID() -> usize;
}

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
}

/// `void MRMediaRemoteGetNowPlayingInfo(dispatch_queue_t, void (^)(CFDictionaryRef))`
type GetNowPlayingInfo = unsafe extern "C" fn(*mut c_void, *mut c_void);

// ------------------------------------------------------------- reading ------

/// The current track, `None` when nothing is playing, or an error when the OS
/// does not expose the API at all.
pub fn current() -> Result<Option<Track>, String> {
    if let Ok(json) = std::env::var("STRATUM_NOWPLAYING_JSON") {
        return parse_json(&json).map(Some);
    }
    query_media_remote()
}

/// Parse the documented JSON shape (also the test seam).
pub fn parse_json(json: &str) -> Result<Track, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        #[serde(default)]
        title: String,
        #[serde(default)]
        artist: String,
        #[serde(default)]
        album: String,
        #[serde(default)]
        elapsed: f64,
        #[serde(default)]
        duration: f64,
        #[serde(default)]
        playing: bool,
    }
    let raw: Raw = serde_json::from_str(json).map_err(|e| format!("now-playing JSON: {e}"))?;
    Ok(Track {
        title: raw.title,
        artist: raw.artist,
        album: raw.album,
        elapsed: raw.elapsed,
        duration: raw.duration,
        playing: raw.playing,
    })
}

fn query_media_remote() -> Result<Option<Track>, String> {
    // SAFETY: both CStrings live until the calls return, `dlopen`/`dlsym` only
    // read them, and the handle is checked for null before use. The transmute is
    // sound because the symbol is the documented
    // `void MRMediaRemoteGetNowPlayingInfo(dispatch_queue_t, void (^)(CFDictionaryRef))`
    // and dlsym returned a non-null pointer for that exact name.
    let (handle, get_info) = unsafe {
        let path = CString::new(FRAMEWORK).expect("static path");
        let handle = dlopen(path.as_ptr(), RTLD_LAZY);
        if handle.is_null() {
            return Err(format!("could not open {FRAMEWORK}"));
        }
        let sym = CString::new("MRMediaRemoteGetNowPlayingInfo").expect("static symbol");
        let raw = dlsym(handle, sym.as_ptr());
        if raw.is_null() {
            return Err(
                "MediaRemote no longer exports MRMediaRemoteGetNowPlayingInfo on this macOS".into(),
            );
        }
        (
            handle,
            std::mem::transmute::<*mut c_void, GetNowPlayingInfo>(raw),
        )
    };
    let _ = handle;

    let (tx, rx) = mpsc::channel::<Option<Track>>();
    // SAFETY: block2 keeps the closure alive for as long as the block exists; the
    // sender is moved into it, so a dropped receiver only makes `send` fail.
    let block = block2::RcBlock::new(move |dict: *const c_void| {
        let track = if dict.is_null() {
            None
        } else {
            // SAFETY: the block is only ever called by MediaRemote with the
            // dictionary it documented, valid for the duration of this call.
            unsafe { read_dict(dict) }
        };
        let _ = tx.send(track);
    });

    // SAFETY: the label is a live CString for the duration of the call and the
    // attributes are null, which libdispatch documents as "default attributes".
    // A private serial queue is serviced by libdispatch's pool, so the callback
    // runs without this process pumping a run loop (which a CLI does not have).
    let queue = unsafe {
        let label = CString::new("dev.stratum.nowplaying").expect("static label");
        dispatch_queue_create(label.as_ptr(), std::ptr::null())
    };
    if queue.is_null() {
        return Err("could not create a dispatch queue".into());
    }

    // SAFETY: `queue` and the block pointer are valid for the duration of the
    // call; MediaRemote copied nothing, and our block outlives the callback
    // because we hold `block` until after `recv` returns.
    unsafe {
        get_info(queue, block2::RcBlock::as_ptr(&block) as *mut c_void);
    }

    match rx.recv_timeout(Duration::from_millis(1500)) {
        Ok(track) => Ok(track),
        Err(_) => Err("MediaRemote did not answer within 1.5 s".into()),
    }
}

/// Extract the fields we care about. The keys are CFString *variables* exported
/// by MediaRemote (`kMRMediaRemoteNowPlayingInfoTitle`, …), so they are looked up
/// by symbol rather than hard-coded as literals — the literals have changed
/// between macOS releases.
unsafe fn read_dict(dict: *const c_void) -> Option<Track> {
    // SAFETY: `dict` is the CFDictionaryRef MediaRemote handed to the block and
    // is valid for the duration of the call; every accessor checks for null and
    // for the value's CFTypeID before reading it.
    let (title, artist, album, duration, elapsed, rate) = unsafe {
        (
            string_value(dict, "kMRMediaRemoteNowPlayingInfoTitle").unwrap_or_default(),
            string_value(dict, "kMRMediaRemoteNowPlayingInfoArtist").unwrap_or_default(),
            string_value(dict, "kMRMediaRemoteNowPlayingInfoAlbum").unwrap_or_default(),
            number_value(dict, "kMRMediaRemoteNowPlayingInfoDuration").unwrap_or(0.0),
            number_value(dict, "kMRMediaRemoteNowPlayingInfoElapsedTime").unwrap_or(0.0),
            // PlaybackRate is 1.0 playing, 0.0 paused, absent when idle.
            number_value(dict, "kMRMediaRemoteNowPlayingInfoPlaybackRate").unwrap_or(0.0),
        )
    };

    if title.is_empty() && artist.is_empty() && album.is_empty() {
        return None;
    }
    Some(Track {
        title,
        artist,
        album,
        elapsed,
        duration,
        playing: rate > 0.0,
    })
}

/// Fetch `kMRMediaRemoteNowPlayingInfo*` constant by symbol, then the value it
/// points at, then the string the dictionary holds under it.
unsafe fn string_value(dict: *const c_void, key_symbol: &str) -> Option<String> {
    // SAFETY: `key` came from `key_ref` (a CFStringRef owned by MediaRemote),
    // `dict` is the live dictionary, and the value's type is checked before it is
    // read as a CFString. The buffer is ours and sized for the maximum path.
    unsafe {
        let key = key_ref(key_symbol)?;
        let value = CFDictionaryGetValue(dict, key);
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let mut buf = [0i8; 1024];
        if !CFStringGetCString(value, buf.as_mut_ptr(), buf.len() as isize, UTF8) {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_string_lossy()
                .into_owned(),
        )
    }
}

unsafe fn number_value(dict: *const c_void, key_symbol: &str) -> Option<f64> {
    // SAFETY: as in `string_value` — the key and dictionary are live CF objects,
    // and the value is only read after its CFTypeID says it is a number or bool.
    // `out` is a local f64 written by CFNumberGetValue with the matching type.
    unsafe {
        let key = key_ref(key_symbol)?;
        let value = CFDictionaryGetValue(dict, key);
        if value.is_null() {
            return None;
        }
        let type_id = CFGetTypeID(value);
        if type_id == CFNumberGetTypeID() {
            let mut out = 0f64;
            let ok = CFNumberGetValue(value, CF_DOUBLE, &mut out as *mut f64 as *mut c_void);
            return ok.then_some(out);
        }
        if type_id == CFBooleanGetTypeID() {
            return Some(if CFBooleanGetValue(value) { 1.0 } else { 0.0 });
        }
        None
    }
}

/// Resolve one of MediaRemote's exported `CFStringRef` constants.
unsafe fn key_ref(symbol: &str) -> Option<*const c_void> {
    // SAFETY: both CStrings outlive the calls; dlsym's result is checked for null
    // before being treated as the address of a `CFStringRef` variable, and the
    // CFStringRef it points at is a constant owned by MediaRemote for the life of
    // the process.
    unsafe {
        let path = CString::new(FRAMEWORK).ok()?;
        let handle = dlopen(path.as_ptr(), RTLD_LAZY);
        if handle.is_null() {
            return None;
        }
        let sym = CString::new(symbol).ok()?;
        let addr = dlsym(handle, sym.as_ptr()) as *const *const c_void;
        if addr.is_null() {
            return None;
        }
        let value = *addr;
        (!value.is_null()).then_some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_seam_parses_the_documented_shape() {
        let t = parse_json(
            r#"{"title":"Teardrop","artist":"Massive Attack","album":"Mezzanine",
                "elapsed":42.5,"duration":330.0,"playing":true}"#,
        )
        .expect("parse");
        assert_eq!(t.title, "Teardrop");
        assert_eq!(t.artist, "Massive Attack");
        assert!(t.playing);
        assert!((t.progress_percent() - 12.878_787).abs() < 0.01);
        assert!((t.remaining() - 287.5).abs() < 0.001);
        assert_eq!(t.one_line(), "Massive Attack — Teardrop");
    }

    #[test]
    fn a_missing_field_defaults_instead_of_failing() {
        let t = parse_json(r#"{"title":"Incomplete"}"#).expect("parse");
        assert_eq!(t.title, "Incomplete");
        assert_eq!(t.artist, "");
        assert_eq!(t.progress_percent(), 0.0);
        assert_eq!(t.one_line(), "Incomplete");
    }

    #[test]
    fn remaining_never_goes_negative() {
        let t = parse_json(r#"{"title":"x","elapsed":400.0,"duration":300.0}"#).expect("parse");
        assert_eq!(t.remaining(), 0.0);
        assert_eq!(t.progress_percent(), 100.0);
    }
}
