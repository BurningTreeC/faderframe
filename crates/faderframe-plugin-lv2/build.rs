fn main() {
    println!("cargo::rerun-if-changed=src/log.c");
    cc::Build::new().file("src/log.c").compile("ff_lv2_log");
}
