use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=native/Platform.swift");
    println!("cargo:rerun-if-changed=native/InputActivity.swift");
    println!("cargo:rerun-if-changed=native/WindowMatching.swift");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
        || env::var_os("CARGO_FEATURE_NATIVE_MACOS").is_none()
    {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let library = out.join("libKagehaPlatform.a");
    let swift_target = if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        "arm64-apple-macosx14.0"
    } else {
        "x86_64-apple-macosx14.0"
    };
    let status = Command::new("xcrun")
        .args([
            "swiftc",
            "-swift-version",
            "5",
            "-target",
            swift_target,
            "-O",
            "-emit-library",
            "-static",
            "-module-name",
            "KagehaPlatform",
            "native/Platform.swift",
            "native/InputActivity.swift",
            "native/WindowMatching.swift",
            "-o",
        ])
        .arg(&library)
        .status()
        .expect("Xcode command line tools with Swift are required");
    assert!(
        status.success(),
        "Swift macOS platform bridge failed to compile"
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=KagehaPlatform");
    for framework in [
        "AppKit",
        "Carbon",
        "ApplicationServices",
        "ScreenCaptureKit",
        "CoreGraphics",
        "Foundation",
        "ImageIO",
        "UniformTypeIdentifiers",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-lib=dylib=swiftCore");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
