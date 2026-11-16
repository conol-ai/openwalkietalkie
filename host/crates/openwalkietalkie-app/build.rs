use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(owt_sparkle)");
    println!("cargo:rerun-if-changed=macos/sparkle_bridge.m");
    println!("cargo:rerun-if-env-changed=OWT_SPARKLE_FRAMEWORK_DIR");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=windows/app.rc");
        println!("cargo:rerun-if-changed=../../../assets/branding/OpenWalkieTalkie.ico");
        // GPUI loads icon resource 1 for the window and taskbar; Explorer
        // reads the same group from the executable.
        embed_resource::compile_for("windows/app.rc", ["openwalkietalkie-app"], embed_resource::NONE)
            .manifest_required()
            .expect("compile the Windows app icon");
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let Some(framework_dir) = env::var_os("OWT_SPARKLE_FRAMEWORK_DIR").map(PathBuf::from) else {
        // Normal source builds deliberately use the no-op Rust implementation.
        // Release packaging supplies the pinned framework directory.
        return;
    };
    validate_framework(&framework_dir);

    let framework_flag = format!("-F{}", framework_dir.display());
    cc::Build::new()
        .file("macos/sparkle_bridge.m")
        .flag("-fobjc-arc")
        .flag("-mmacosx-version-min=13.0")
        .flag(&framework_flag)
        .compile("owt_sparkle_bridge");

    println!("cargo:rustc-cfg=owt_sparkle");
    println!(
        "cargo:rustc-link-search=framework={}",
        framework_dir.display()
    );
    println!("cargo:rustc-link-lib=framework=Sparkle");
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
}

fn validate_framework(parent: &Path) {
    let framework = parent.join("Sparkle.framework");
    let header = framework.join("Headers/Sparkle.h");
    let binary = framework.join("Versions/B/Sparkle");
    if !header.is_file() || !binary.is_file() {
        panic!(
            "OWT_SPARKLE_FRAMEWORK_DIR must contain a complete Sparkle.framework: {}",
            parent.display()
        );
    }
}
