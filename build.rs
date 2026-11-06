fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let out_dir = std::path::Path::new(&out);
    std::fs::copy("memory.x", out_dir.join("memory.x")).unwrap();
    std::fs::copy("psram.x", out_dir.join("psram.x")).unwrap();
    // A temporary bootloader updater must fit in protocol-1 recovery's lower
    // bank so it can install protocol 2 before any upper-bank assets are used.
    let bootstrap = std::env::var_os("CARGO_FEATURE_BOOTLOADER_UPDATE").is_some();
    let assets = if bootstrap { "flash-bootstrap.x" } else { "flash-assets.x" };
    std::fs::copy(assets, out_dir.join(assets)).unwrap();
    println!("cargo:rustc-link-search={out}");
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=psram.x");
    println!("cargo:rerun-if-changed=flash-assets.x");
    println!("cargo:rerun-if-changed=flash-bootstrap.x");

    // Section addresses are not 0x10000-aligned; without --nmagic the linker
    // pads them out to page boundaries and the image stops fitting.
    println!("cargo:rustc-link-arg=--nmagic");
    // Every board executes in place from NOR (cortex-m-rt's link.x).
    println!("cargo:rustc-link-arg=-Tlink.x");
    println!("cargo:rustc-link-arg=-T{assets}");
    println!("cargo:rustc-link-arg=-Tpsram.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}
