use crate::args::{Architecture, ArchitectureSelection, QemuTestArgs};
use crate::process::{self, CapturedProcess};
use crate::tap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const STANDARD_SUITES: [&str; 8] = [
    "smoke",
    "kernel-core",
    "isr",
    "allocator",
    "static-only",
    "port",
    "tickless",
    "safe-api",
];

pub fn run(repository: &Path, arguments: QemuTestArgs) -> Result<(), String> {
    let timeout = timeout_from_environment()?;
    let architectures: Vec<_> = match arguments.arch {
        ArchitectureSelection::One(arch) => vec![arch],
        ArchitectureSelection::All => Architecture::SINGLE_CORE.to_vec(),
    };

    let mut failures = Vec::new();
    let mut completed = 0;
    for arch in architectures {
        let suites: Vec<&str> = if arguments.suite == "all" {
            if arch == Architecture::CortexA9Smp {
                vec!["smp"]
            } else {
                STANDARD_SUITES.to_vec()
            }
        } else {
            vec![arguments.suite.as_str()]
        };

        for suite in suites {
            match run_one(repository, arch, suite, arguments.cpus, timeout) {
                Ok(test_count) => {
                    completed += 1;
                    println!("PASS {arch}/{suite} ({test_count} tests)");
                }
                Err(error) => failures.push(format!("{arch}/{suite}: {error}")),
            }
        }
    }

    if failures.is_empty() {
        println!("QEMU test matrix passed: {completed} firmware run(s)");
        Ok(())
    } else {
        Err(format!(
            "QEMU test matrix failed ({} failure(s), {completed} passed):\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        ))
    }
}

fn run_one(
    repository: &Path,
    arch: Architecture,
    suite: &str,
    requested_cpus: Option<usize>,
    timeout: Duration,
) -> Result<usize, String> {
    let platform = Platform::for_arch(arch);
    let firmware_directory = repository
        .join("tests/qemu")
        .join(platform.firmware_directory);
    let manifest = firmware_directory.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(format!(
            "test firmware is not present; expected {}",
            display_path(repository, &manifest)
        ));
    }

    let cpus = requested_cpus.unwrap_or(platform.default_cpus);
    if cpus > platform.maximum_cpus {
        return Err(format!(
            "requested {cpus} CPUs, but the {arch} test machine supports at most {}",
            platform.maximum_cpus
        ));
    }
    if arch == Architecture::CortexA9Smp && cpus < 2 {
        return Err("the cortex-a9-smp target requires at least two CPUs".to_owned());
    }

    let target_directory = repository.join("target/xtask/qemu").join(arch.as_str());
    println!("Building {arch}/{suite}...");
    build_firmware(
        repository,
        &firmware_directory,
        &manifest,
        &target_directory,
        platform.target,
        suite,
    )?;

    let firmware = target_directory
        .join(platform.target)
        .join("release")
        .join(suite);
    if !firmware.is_file() {
        return Err(format!(
            "cargo succeeded, but firmware artifact is missing: {}",
            display_path(repository, &firmware)
        ));
    }

    let log_directory = repository.join("target/xtask/qemu-logs");
    fs::create_dir_all(&log_directory).map_err(|error| {
        format!(
            "could not create QEMU log directory {}: {error}",
            log_directory.display()
        )
    })?;
    let log_path = log_directory.join(format!("{}-{suite}.log", arch.as_str()));

    println!(
        "Running {arch}/{suite} in QEMU (timeout {}s, log {})...",
        timeout.as_secs_f64(),
        display_path(repository, &log_path)
    );
    let mut command = platform.qemu_command(&firmware, cpus);
    let output = process::run_captured(&mut command, timeout, true)?;
    write_log(&log_path, arch, suite, cpus, &command, &output)?;

    validate_process(repository, &log_path, output)
}

fn build_firmware(
    repository: &Path,
    firmware_directory: &Path,
    manifest: &Path,
    target_directory: &Path,
    target: &str,
    suite: &str,
) -> Result<(), String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let status = Command::new(&cargo)
        .current_dir(firmware_directory)
        .arg("build")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target")
        .arg(target)
        .arg("--target-dir")
        .arg(target_directory)
        .arg("--release")
        .arg("--bin")
        .arg(suite)
        .status()
        .map_err(|error| format!("could not start {:?}: {error}", cargo))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "firmware build failed with {status}; manifest {} must provide a `{suite}` binary",
            display_path(repository, manifest)
        ))
    }
}

fn validate_process(
    repository: &Path,
    log_path: &Path,
    output: CapturedProcess,
) -> Result<usize, String> {
    let log = display_path(repository, log_path);
    if output.timed_out {
        return Err(format!(
            "QEMU timed out after {:.2}s without a final result; full log: {log}\n{}",
            output.elapsed.as_secs_f64(),
            diagnostic_tail(&output)
        ));
    }
    if !output.status.success() && !output.terminated_after_result {
        return Err(format!(
            "QEMU exited with {} before reporting a final result; full log: {log}\n{}",
            output.status,
            diagnostic_tail(&output)
        ));
    }

    match tap::parse(&output.stdout, &output.stderr) {
        Ok(report) => Ok(report.test_count),
        Err(errors) => Err(format!(
            "invalid or failing guest test output; full log: {log}\n  - {}\n{}",
            errors.join("\n  - "),
            diagnostic_tail(&output)
        )),
    }
}

fn timeout_from_environment() -> Result<Duration, String> {
    match env::var("FREERUSTOS_QEMU_TIMEOUT_SECS") {
        Ok(value) => {
            let seconds = value.parse::<u64>().map_err(|_| {
                format!("invalid FREERUSTOS_QEMU_TIMEOUT_SECS={value:?}; expected 1..=300")
            })?;
            if !(1..=300).contains(&seconds) {
                return Err(format!(
                    "invalid FREERUSTOS_QEMU_TIMEOUT_SECS={value:?}; expected 1..=300"
                ));
            }
            Ok(Duration::from_secs(seconds))
        }
        Err(env::VarError::NotPresent) => Ok(Duration::from_secs(25)),
        Err(error) => Err(format!("could not read QEMU timeout: {error}")),
    }
}

fn write_log(
    path: &Path,
    arch: Architecture,
    suite: &str,
    cpus: usize,
    command: &Command,
    output: &CapturedProcess,
) -> Result<(), String> {
    let mut file = fs::File::create(path)
        .map_err(|error| format!("could not create QEMU log {}: {error}", path.display()))?;
    writeln!(file, "architecture: {arch}").map_err(log_write_error)?;
    writeln!(file, "suite: {suite}").map_err(log_write_error)?;
    writeln!(file, "cpus: {cpus}").map_err(log_write_error)?;
    writeln!(file, "command: {command:?}").map_err(log_write_error)?;
    writeln!(file, "status: {}", output.status).map_err(log_write_error)?;
    writeln!(file, "elapsed_seconds: {:.3}", output.elapsed.as_secs_f64())
        .map_err(log_write_error)?;
    writeln!(file, "timed_out: {}", output.timed_out).map_err(log_write_error)?;
    writeln!(
        file,
        "terminated_after_result: {}\n\n[stdout]",
        output.terminated_after_result
    )
    .map_err(log_write_error)?;
    file.write_all(&output.stdout).map_err(log_write_error)?;
    file.write_all(b"\n\n[stderr]\n").map_err(log_write_error)?;
    file.write_all(&output.stderr).map_err(log_write_error)?;
    Ok(())
}

fn log_write_error(error: std::io::Error) -> String {
    format!("could not write QEMU log: {error}")
}

fn diagnostic_tail(output: &CapturedProcess) -> String {
    let mut text = String::new();
    text.push_str("--- guest stdout/stderr tail ---\n");
    text.push_str(&tail(&String::from_utf8_lossy(&output.stdout), 50));
    if !output.stderr.is_empty() {
        text.push_str("\n--- QEMU stderr tail ---\n");
        text.push_str(&tail(&String::from_utf8_lossy(&output.stderr), 50));
    }
    text
}

fn tail(text: &str, maximum_lines: usize) -> String {
    let lines: Vec<_> = text.lines().collect();
    let start = lines.len().saturating_sub(maximum_lines);
    lines[start..].join("\n")
}

fn display_path(repository: &Path, path: &Path) -> String {
    path.strip_prefix(repository)
        .unwrap_or(path)
        .display()
        .to_string()
}

struct Platform {
    firmware_directory: &'static str,
    target: &'static str,
    qemu: &'static str,
    machine: &'static str,
    cpu: Option<&'static str>,
    memory: Option<&'static str>,
    semihosting: bool,
    default_cpus: usize,
    maximum_cpus: usize,
}

impl Platform {
    fn for_arch(arch: Architecture) -> Self {
        match arch {
            Architecture::CortexM0 => {
                Self::arm_m("cortex-m0", "thumbv6m-none-eabi", "microbit", "cortex-m0")
            }
            Architecture::CortexM3 => {
                Self::arm_m("cortex-m3", "thumbv7m-none-eabi", "mps2-an385", "cortex-m3")
            }
            Architecture::CortexM4f => Self::arm_m(
                "cortex-m4f",
                "thumbv7em-none-eabihf",
                "mps2-an386",
                "cortex-m4",
            ),
            Architecture::CortexM7 => Self::arm_m(
                "cortex-m7",
                "thumbv7em-none-eabihf",
                "mps2-an500",
                "cortex-m7",
            ),
            Architecture::Riscv32 => Self {
                firmware_directory: "riscv32",
                target: "riscv32imac-unknown-none-elf",
                qemu: "qemu-system-riscv32",
                machine: "sifive_e",
                cpu: None,
                memory: None,
                semihosting: false,
                default_cpus: 1,
                maximum_cpus: 1,
            },
            Architecture::CortexA9 | Architecture::CortexA9Smp => Self {
                firmware_directory: "cortex-a9",
                target: "armv7a-none-eabi",
                qemu: "qemu-system-arm",
                machine: "vexpress-a9",
                cpu: Some("cortex-a9"),
                memory: Some("128M"),
                semihosting: true,
                default_cpus: if arch == Architecture::CortexA9Smp {
                    2
                } else {
                    1
                },
                maximum_cpus: if arch == Architecture::CortexA9Smp {
                    4
                } else {
                    1
                },
            },
            Architecture::CortexA53 => Self {
                firmware_directory: "cortex-a53",
                target: "aarch64-unknown-none",
                qemu: "qemu-system-aarch64",
                machine: "virt",
                cpu: Some("cortex-a53"),
                memory: Some("128M"),
                semihosting: true,
                default_cpus: 1,
                maximum_cpus: 1,
            },
        }
    }

    fn arm_m(
        firmware_directory: &'static str,
        target: &'static str,
        machine: &'static str,
        cpu: &'static str,
    ) -> Self {
        Self {
            firmware_directory,
            target,
            qemu: "qemu-system-arm",
            machine,
            cpu: Some(cpu),
            memory: None,
            semihosting: true,
            default_cpus: 1,
            maximum_cpus: 1,
        }
    }

    fn qemu_command(&self, firmware: &Path, cpus: usize) -> Command {
        let mut command = Command::new(self.qemu);
        command.arg("-M").arg(self.machine);
        if let Some(cpu) = self.cpu {
            command.arg("-cpu").arg(cpu);
        }
        if let Some(memory) = self.memory {
            command.arg("-m").arg(memory);
        }
        if cpus > 1 {
            command.arg("-smp").arg(cpus.to_string());
        }
        command.args([
            OsStr::new("-display"),
            OsStr::new("none"),
            OsStr::new("-monitor"),
            OsStr::new("none"),
            OsStr::new("-serial"),
            OsStr::new("stdio"),
            OsStr::new("-no-reboot"),
        ]);
        if self.semihosting {
            command
                .arg("-semihosting-config")
                .arg("enable=on,target=native");
        }
        command.arg("-kernel").arg(firmware);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cortex_m4f_mapping_matches_the_qemu_test_machine() {
        let platform = Platform::for_arch(Architecture::CortexM4f);
        let command = platform.qemu_command(Path::new("firmware"), 1);
        let rendered = format!("{command:?}");
        assert!(rendered.contains("qemu-system-arm"));
        assert!(rendered.contains("mps2-an386"));
        assert!(rendered.contains("cortex-m4"));
        assert!(rendered.contains("semihosting-config"));
    }

    #[test]
    fn smp_mapping_passes_the_cpu_count() {
        let platform = Platform::for_arch(Architecture::CortexA9Smp);
        let command = platform.qemu_command(Path::new("firmware"), 2);
        let rendered = format!("{command:?}");
        assert!(rendered.contains("vexpress-a9"));
        assert!(rendered.contains("-smp"));
        assert!(rendered.contains('2'));
    }

    #[test]
    fn tail_is_bounded() {
        assert_eq!(tail("one\ntwo\nthree", 2), "two\nthree");
    }
}
