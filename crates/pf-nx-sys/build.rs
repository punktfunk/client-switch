//! On the console target: builds `csrc/pf_swkbd.c`, and `csrc/pf_av.c` with `av`, and links libnx, newlib, the pinned libavcodec and
//! NVK. On Linux with `av`: the same shim against the system's libavcodec, which decodes in
//! software for the dev box. Elsewhere: nothing. `cfg(pf_av)` says the shim is linked.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(pf_av)");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if os == "linux" && env::var_os("CARGO_FEATURE_AV").is_some() {
        build_dev_box_shim();
        return;
    }
    if os != "horizon" {
        return;
    }
    println!("cargo:rerun-if-env-changed=DEVKITPRO");
    println!("cargo:rerun-if-env-changed=PF_NX_NVK_LIB");
    let dkp = PathBuf::from(env::var_os("DEVKITPRO").unwrap_or_else(|| "/opt/devkitpro".into()));
    let portlibs = dkp.join("portlibs/switch");
    let av = env::var_os("CARGO_FEATURE_AV").is_some();
    let nvk_on = env::var_os("CARGO_FEATURE_NVK").is_some();

    if av {
        build_shim(&dkp, &portlibs, "pf_av");
        println!("cargo:rustc-cfg=pf_av");
    }
    build_shim(&dkp, &portlibs, "pf_swkbd");

    let nvk = PathBuf::from(env::var_os("PF_NX_NVK_LIB").unwrap_or_else(|| "/opt/pf-nx/nvk/lib".into()));
    if nvk_on && !nvk.join("libvulkan_bh.a").is_file() {
        println!(
            "cargo:warning=no libvulkan_bh.a in {}: run scripts/build-nvk.sh or set PF_NX_NVK_LIB",
            nvk.display()
        );
    }

    let mut dirs = vec![
        nvk,
        dkp.join("libnx/lib"),
        portlibs.join("lib"),
        dkp.join("devkitA64/aarch64-none-elf/lib/pic"),
    ];
    // devkitA64's libgcc, under its GCC version: lib/gcc/aarch64-none-elf/<version>/pic.
    if let Ok(versions) = std::fs::read_dir(dkp.join("devkitA64/lib/gcc/aarch64-none-elf")) {
        dirs.extend(versions.flatten().map(|e| e.path().join("pic")).filter(|p| p.is_dir()));
    }
    for dir in dirs {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    // Dependents first. NVK needs expat, zstd, zlib and libstdc++ (bevy-horizon's
    // localize_nvk.sh leaves exactly those undefined).
    let mut libs: Vec<&str> = Vec::new();
    if av {
        // `pkg-config --static --libs libavcodec` for the image's switch-ffmpeg (libavcodec
        // 61.19): its codec registry pulls every decoder, so dav1d and swresample come along.
        libs.extend(["avcodec", "dav1d", "swresample", "avutil", "z"]);
    }
    if nvk_on {
        libs.extend(["vulkan_bh", "expat", "zstd", "z"]);
    }
    // Skia is C++ too, and names no system library on this target: libstdc++ links either way.
    libs.extend(["stdc++", "nx", "sysbase", "c", "m", "gcc"]);
    for lib in libs {
        println!("cargo:rustc-link-lib={lib}");
    }
}

/// `csrc/{name}.c` with devkitA64's GCC and libnx's flags, unless the toolchain's cc variables
/// name a compiler.
fn build_shim(dkp: &Path, portlibs: &Path, name: &str) {
    let file = format!("csrc/{name}.c");
    println!("cargo:rerun-if-changed={file}");
    let target = env::var("TARGET").unwrap_or_default().replace('-', "_");
    let mut build = cc::Build::new();
    if env::var_os(format!("CC_{target}")).is_none() && env::var_os("TARGET_CC").is_none() {
        build.compiler(dkp.join("devkitA64/bin/aarch64-none-elf-gcc"));
    }
    if env::var_os(format!("CFLAGS_{target}")).is_none() && env::var_os("TARGET_CFLAGS").is_none() {
        // bevy-horizon docs/toolchain.md: the flags libnx's own Makefiles build with.
        for flag in ["-march=armv8-a+crc+crypto", "-mtune=cortex-a57", "-mtp=soft", "-fPIE"] {
            build.flag(flag);
        }
    }
    build
        .file(&file)
        .include(portlibs.join("include"))
        .include(dkp.join("libnx/include"))
        .define("__SWITCH__", None)
        .warnings(true)
        .compile(name);
}

/// The shim against the system's libavcodec (`libavcodec-dev` on Debian and Ubuntu).
fn build_dev_box_shim() {
    println!("cargo:rerun-if-changed=csrc/pf_av.c");
    let mut build = cc::Build::new();
    let mut found = Vec::new();
    for lib in ["libavcodec", "libavutil"] {
        let lib = pkg_config::Config::new()
            .cargo_metadata(false)
            .probe(lib)
            .unwrap_or_else(|e| panic!("{lib} for the dev box's software decoder (libavcodec-dev): {e}"));
        for dir in &lib.include_paths {
            build.include(dir);
        }
        found.push(lib);
    }
    build.file("csrc/pf_av.c").warnings(true).compile("pf_av");
    // After the shim: with --as-needed a library named before its user is dropped.
    for lib in found {
        for dir in lib.link_paths {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
        for name in lib.libs {
            println!("cargo:rustc-link-lib={name}");
        }
    }
    println!("cargo:rustc-cfg=pf_av");
}
