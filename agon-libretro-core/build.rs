use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../src/vdp/rust_glue.cpp");
    println!("cargo:rerun-if-changed=../src/vdp/vdp-console8.cpp");

    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set by Cargo");
    let info = format!(
        "display_name = \"Fab Agon\"\n\
corename = \"Fab Agon\"\n\
manufacturer = \"The Byte Atic\"\n\
systemname = \"Agon Light\"\n\
systemid = \"agon\"\n\
authors = \"Tom Morton\"\n\
supported_extensions = \"bin|bas|bbc\"\n\
firmware_count = 2\n\
firmware0_desc = \"Agon Console8 MOS\"\n\
firmware0_path = \"agon/mos_console8.bin\"\n\
firmware0_opt = \"false\"\n\
firmware1_desc = \"Agon Platform VDP\"\n\
firmware1_path = \"agon/vdp_console8.dll\"\n\
firmware1_opt = \"false\"\n\
categories = \"Emulator\"\n\
database = \"Agon Light\"\n\
license = \"GPLv3\"\n\
permissions = \"\"\n\
display_version = \"{version}\"\n"
    );

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .expect("Cargo build output has a profile directory");
    fs::write(profile_dir.join("agon_libretro.info"), info)
        .expect("failed to write agon_libretro.info");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let status = Command::new("make")
            .args(["-C", "../src/vdp", "vdp_console8.so"])
            .status()
            .expect("failed to run make for the Agon VDP firmware");
        assert!(status.success(), "failed to build the Agon VDP firmware");
        fs::copy(
            "../src/vdp/vdp_console8.so",
            profile_dir.join("vdp_console8.dll"),
        )
        .expect("failed to copy vdp_console8.dll");
    }
}
