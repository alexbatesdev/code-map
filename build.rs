use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native/megamap.m");
    println!("cargo:rerun-if-changed=native/megamap.h");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") { return; }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let object = out.join("megamap_native.o");
    let library = out.join("libmegamap_native.a");
    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64", Ok("x86_64") => "x86_64", _ => panic!("unsupported macOS architecture"),
    };
    let status = Command::new("xcrun").args(["clang", "-fobjc-arc", "-O2", "-Wall", "-Wextra", "-Werror", "-arch", architecture, "-mmacosx-version-min=11.0", "-c", "native/megamap.m", "-o"]).arg(&object).status().expect("Apple clang is required");
    assert!(status.success(), "native Metal backend did not compile");
    let status = Command::new("ar").arg("crs").arg(&library).arg(&object).status().expect("ar is required");
    assert!(status.success(), "native Metal backend archive failed");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=megamap_native");
    for framework in ["AppKit", "Metal", "QuartzCore", "CoreText", "ImageIO"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
