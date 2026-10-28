//! On the console target: builds `csrc/pf_av.c` and links libnx, newlib, the pinned libavcodec and
//! NVK. Off it: nothing.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("horizon") {
        return;
    }
    println!("cargo:rerun-if-env-changed=DEVKITPRO");
    println!("cargo:rerun-if-env-changed=PF_NX_NVK_LIB");
    let dkp = PathBuf::from(env::var_os("DEVKITPRO").unwrap_or_else(|| "/opt/devkitpro".into()));
    let portlibs = dkp.join("portlibs/switch");
    let av = env::var_os("CARGO_FEATURE_AV").is_some();
    let nvk_on = env::var_os("CARGO_FEATURE_NVK").is_some();

    if av {
        build_shim(&dkp, &portlibs);
    }

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
    // TODO(verify): scripts/build-ffmpeg.sh builds --disable-autodetect, so avcodec and avutil
    // need nothing beyond libnx and newlib.
    let mut libs: Vec<&str> = Vec::new();
    if av {
        libs.extend(["avcodec", "avutil"]);
    }
    if nvk_on {
        libs.extend(["vulkan_bh", "expat", "zstd", "z", "stdc++"]);
    }
    libs.extend(["nx", "sysbase", "c", "m", "gcc"]);
    for lib in libs {
        println!("cargo:rustc-link-lib={lib}");
    }
}

/// devkitA64's GCC with libnx's flags, unless the toolchain's cc variables name a compiler.
fn build_shim(dkp: &Path, portlibs: &Path) {
    println!("cargo:rerun-if-changed=csrc/pf_av.c");
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
        .file("csrc/pf_av.c")
        .include(portlibs.join("include"))
        .include(dkp.join("libnx/include"))
        .define("__SWITCH__", None)
        .warnings(true)
        .compile("pf_av");
}
