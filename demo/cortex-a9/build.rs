use std::env;
use std::path::PathBuf;

fn main() {
    // Get the output directory
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Tell Cargo where to find the linker script
    println!("cargo:rustc-link-search={}", out_dir.display());
    println!(
        "cargo:rustc-link-search={}",
        env::var("CARGO_MANIFEST_DIR").unwrap()
    );

    // Rerun if linker script changes
    println!("cargo:rerun-if-changed=link.x");
    println!("cargo:rerun-if-changed=src/startup.S");

    // Compile the startup assembly
    cc::Build::new()
        .file("src/startup.S")
        .flag("-mcpu=cortex-a9")
        .flag("-marm")
        .flag("-mfpu=neon-vfpv3")
        .flag("-mfloat-abi=hard")
        .compile("startup");
}
