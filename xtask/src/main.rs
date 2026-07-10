mod args;
mod features;
mod process;
mod qemu;
mod tap;

use args::Action;
use std::env;
use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = real_main() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), String> {
    let action = args::parse(env::args().skip(1))?;
    if action == Action::Help {
        print!("{}", args::usage());
        return Ok(());
    }

    let repository = repository_root()?;
    match action {
        Action::Help => Ok(()),
        Action::TestHost => features::test_host(&repository),
        Action::CheckFeatures => features::check(&repository),
        Action::QemuTest(arguments) => qemu::run(&repository, arguments),
    }
}

fn repository_root() -> Result<PathBuf, String> {
    let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repository = manifest_directory
        .parent()
        .ok_or_else(|| "xtask manifest has no parent directory".to_owned())?;
    if !repository.join("Cargo.toml").is_file() {
        return Err(format!(
            "could not find project Cargo.toml above {}",
            manifest_directory.display()
        ));
    }
    Ok(repository.to_owned())
}
