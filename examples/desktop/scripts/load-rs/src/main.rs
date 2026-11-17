//! A stratum script in Rust: print `key=value` lines on stdout.
//!
//! Run by the runtime as `cargo run --quiet --manifest-path <this dir>/Cargo.toml`,
//! so it compiles once and is cached. A release build or a prebuilt binary works
//! too — point `file` at the executable and it is run directly.

fn main() {
    let loads = load_average();
    let series = loads
        .iter()
        .map(|v| format!("{v:.2}"))
        .collect::<Vec<_>>()
        .join(",");

    // One key per line. `load` feeds the line graph, `load_text` the label.
    println!("load={series}");
    println!("load_text={:.2}", loads[0]);
}

/// 1/5/15 minute load averages from `getloadavg(3)`.
fn load_average() -> [f64; 3] {
    let mut loads = [0.0f64; 3];
    let n = unsafe { getloadavg(loads.as_mut_ptr(), 3) };
    if n <= 0 { [0.0; 3] } else { loads }
}

unsafe extern "C" {
    fn getloadavg(loadavg: *mut f64, nelem: std::ffi::c_int) -> std::ffi::c_int;
}
