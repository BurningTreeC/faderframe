//! Compiles the vendored Signalsmith Stretch behind a small C shim, and a
//! C++ allocation counter linked into this crate's test binaries only.

fn main() {
    println!("cargo::rerun-if-changed=src/shim.cpp");
    println!("cargo::rerun-if-changed=src/count_new.cpp");
    println!("cargo::rerun-if-changed=vendor");
    println!("cargo::rerun-if-env-changed=FADERFRAME_CHECK_ONLY");
    println!("cargo::rustc-check-cfg=cfg(ff_cpp_count)");
    // `cargo check` for a target without a C++ toolchain (cross-checking
    // the Windows and macOS code paths): nothing is linked, so skip it.
    if std::env::var_os("FADERFRAME_CHECK_ONLY").is_some() {
        return;
    }
    cc::Build::new()
        .cpp(true)
        .std("c++14")
        .file("src/shim.cpp")
        .include("vendor")
        .opt_level(3)
        .flag_if_supported("-ffp-contract=fast")
        .warnings(false)
        .compile("faderframe_stretch");
    // The allocation counter replaces the global operator new in the test
    // binaries. MinGW's linker resolves libraries before a raw object, so
    // Windows tests go without it (they still run the stretcher).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        return;
    }
    println!("cargo::rustc-cfg=ff_cpp_count");
    let objects = cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("src/count_new.cpp")
        .compile_intermediates();
    for o in objects {
        println!("cargo::rustc-link-arg-tests={}", o.display());
    }
}
