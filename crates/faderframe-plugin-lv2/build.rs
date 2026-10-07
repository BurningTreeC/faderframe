//! Compiles the log feature's printf shim (`src/log.c`).

fn main() {
    println!("cargo::rerun-if-changed=src/log.c");
    println!("cargo::rerun-if-env-changed=FADERFRAME_CHECK_ONLY");
    // `cargo check` for a target without its C toolchain (cross-checking
    // other platforms): nothing is linked, so skip it.
    if std::env::var_os("FADERFRAME_CHECK_ONLY").is_some() {
        return;
    }
    cc::Build::new().file("src/log.c").compile("ff_lv2_log");
}
