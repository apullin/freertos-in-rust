//! Build script for FreeRTOS demo
//!
//! Copies memory.x to the output directory so the linker can find it.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    // Get the output directory
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Put memory.x in the output directory
    File::create(out_dir.join("memory.x"))
        .unwrap()
        .write_all(include_bytes!("memory.x"))
        .unwrap();

    // Tell cargo to look for linker scripts in the output directory
    println!("cargo:rustc-link-search={}", out_dir.display());

    // Rebuild if memory.x changes
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
}
