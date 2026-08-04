use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn remove_target_files(directory: &Path, suffix: &str) {
    let entries = fs::read_dir(directory).expect("failed to inspect VDP build directory");
    for entry in entries {
        let entry = entry.expect("failed to inspect VDP build entry");
        let path = entry.path();
        if path.is_dir() {
            remove_target_files(&path, suffix);
        } else if entry.file_name().to_string_lossy().contains(suffix) {
            fs::remove_file(&path).expect("failed to remove a target-specific VDP build file");
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../src/vdp");
    println!("cargo:rerun-if-changed=../firmware/mos_console8.bin");

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo target OS is set");
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture is set");
    let suffix = format!(".libretro-{target_os}-{target_arch}");
    let vdp_dir = Path::new("../src/vdp");
    let make_target = format!("vdp_console8{suffix}.so");

    let mut make = Command::new(env::var_os("MAKE").unwrap_or_else(|| "make".into()));
    make.arg("-C").arg(vdp_dir).arg(&make_target);
    make.arg(format!("SUFFIX={suffix}"));
    if target_os == "windows" {
        make.arg("OS=Windows_NT");
    }

    let status = make
        .status()
        .expect("failed to run make for the bundled Agon VDP");
    assert!(status.success(), "failed to build the bundled Agon VDP");

    let extension = match target_os.as_str() {
        "windows" => "dll",
        "macos" => "dylib",
        _ => "so",
    };
    let source = vdp_dir.join(make_target);
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR is set"));
    let bundled_vdp = out_dir.join(format!("vdp_console8.{extension}"));
    fs::copy(&source, &bundled_vdp).expect("failed to stage the bundled Agon VDP");

    let bytes = fs::read(&bundled_vdp).expect("failed to read the bundled Agon VDP");
    let hash = bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    remove_target_files(vdp_dir, &suffix);
    println!("cargo:rustc-env=AGON_BUNDLED_VDP={}", bundled_vdp.display());
    println!("cargo:rustc-env=AGON_BUNDLED_VDP_HASH={hash:016x}");
}
