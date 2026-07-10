use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

const ALLOCATORS: [Option<&str>; 4] = [None, Some("heap-4"), Some("heap-5"), Some("alloc")];

const CAPABILITIES: [&str; 22] = [
    "list-data-integrity-check",
    "trace-facility",
    "stack-overflow-check",
    "record-stack-high-address",
    "task-delete",
    "task-suspend",
    "abort-delay",
    "task-priority-set",
    "stack-high-water-mark",
    "user-config",
    "use-mutexes",
    "timers",
    "stream-buffers",
    "queue-sets",
    "queue-registry",
    "pend-function-call",
    "application-task-tag",
    "thread-local-storage",
    "idle-yield",
    "stats-formatting",
    "generate-run-time-stats",
    "tickless-idle",
];

struct Port {
    feature: &'static str,
    target: Option<&'static str>,
}

const PORTS: [Port; 9] = [
    Port {
        feature: "port-test",
        target: None,
    },
    Port {
        feature: "port-dummy",
        target: None,
    },
    Port {
        feature: "port-cortex-m0",
        target: Some("thumbv6m-none-eabi"),
    },
    Port {
        feature: "port-cortex-m3",
        target: Some("thumbv7m-none-eabi"),
    },
    Port {
        feature: "port-cortex-m4f",
        target: Some("thumbv7em-none-eabihf"),
    },
    Port {
        feature: "port-cortex-m7",
        target: Some("thumbv7em-none-eabihf"),
    },
    Port {
        feature: "port-riscv32",
        target: Some("riscv32imac-unknown-none-elf"),
    },
    Port {
        feature: "port-cortex-a9",
        target: Some("armv7a-none-eabi"),
    },
    Port {
        feature: "port-cortex-a53",
        target: Some("aarch64-unknown-none"),
    },
];

pub fn test_host(repository: &Path) -> Result<(), String> {
    let features = defined_features(repository)?;
    let port = if features.contains("port-test") {
        "port-test"
    } else {
        "port-dummy"
    };
    let mut selected = vec!["std", port, "alloc"];
    if features.contains("all-kernel-features") {
        selected.push("all-kernel-features");
    }

    for release in [false, true] {
        let description = if release {
            "host tests (release)"
        } else {
            "host tests (debug)"
        };
        let mut command = cargo(repository);
        command
            .arg("test")
            .arg("--no-default-features")
            .arg("--features")
            .arg(selected.join(","));
        if release {
            command.arg("--release");
        }
        command.args(["--", "--test-threads=1"]);
        run_positive(description, &mut command)?;
    }
    Ok(())
}

pub fn check(repository: &Path) -> Result<(), String> {
    let defined = defined_features(repository)?;
    let mut checks = 0;

    for port in PORTS.iter().filter(|port| defined.contains(port.feature)) {
        for allocator in ALLOCATORS {
            let mut features = vec![port.feature];
            if port.feature == "port-test" {
                features.push("std");
            }
            if let Some(allocator) = allocator {
                features.push(allocator);
            }
            let name = allocator.unwrap_or("static-only");
            let mut command = check_command(repository, &features, port.target);
            run_positive(&format!("{} + {name}", port.feature), &mut command)?;
            checks += 1;
        }
    }

    for capability in CAPABILITIES
        .iter()
        .copied()
        .filter(|feature| defined.contains(*feature))
    {
        let mut command =
            check_command(repository, &["port-test", "std", "alloc", capability], None);
        run_positive(&format!("individual capability {capability}"), &mut command)?;
        checks += 1;
    }

    let maximal = if defined.contains("all-kernel-features") {
        vec!["port-test", "std", "alloc", "all-kernel-features"]
    } else {
        let mut features = vec!["port-test", "std", "alloc"];
        features.extend(
            CAPABILITIES
                .iter()
                .copied()
                .filter(|feature| defined.contains(*feature)),
        );
        features
    };
    let mut command = check_command(repository, &maximal, None);
    run_positive("maximal compatible host configuration", &mut command)?;
    checks += 1;

    let docs_port = if defined.contains("port-dummy") {
        "port-dummy"
    } else {
        "port-test"
    };
    let mut docs_features = vec![docs_port, "alloc"];
    if defined.contains("all-kernel-features") {
        docs_features.push("all-kernel-features");
    }
    let mut docs = cargo(repository);
    docs.arg("doc")
        .arg("--no-deps")
        .arg("--no-default-features")
        .arg("--features")
        .arg(docs_features.join(","))
        .env("RUSTDOCFLAGS", "-D warnings");
    run_positive("canonical docs configuration", &mut docs)?;
    checks += 1;

    let negative_cases: [(&str, &[&str]); 5] = [
        ("no port", &[]),
        ("multiple ports", &["port-dummy", "port-test", "std"]),
        ("multiple allocators", &["port-dummy", "heap-4", "heap-5"]),
        (
            "multiple architecture widths",
            &["port-dummy", "arch-64bit"],
        ),
        ("multiple tick widths", &["port-dummy", "tick-64bit"]),
    ];
    for (description, features) in negative_cases {
        let mut command = check_command(repository, features, None);
        run_expected_failure(description, &mut command)?;
        checks += 1;
    }

    println!("Feature matrix passed: {checks} configurations");
    Ok(())
}

fn defined_features(repository: &Path) -> Result<BTreeSet<String>, String> {
    let manifest = repository.join("Cargo.toml");
    let contents = fs::read_to_string(&manifest)
        .map_err(|error| format!("could not read {}: {error}", manifest.display()))?;
    Ok(parse_features(&contents))
}

fn parse_features(manifest: &str) -> BTreeSet<String> {
    let mut in_features = false;
    let mut features = BTreeSet::new();
    for raw_line in manifest.lines() {
        let line = raw_line.trim();
        if line == "[features]" {
            in_features = true;
            continue;
        }
        if in_features && line.starts_with('[') {
            break;
        }
        if !in_features || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            let name = name.trim();
            if !name.is_empty() {
                features.insert(name.to_owned());
            }
        }
    }
    features
}

fn check_command(repository: &Path, features: &[&str], target: Option<&str>) -> Command {
    let mut command = cargo(repository);
    command.arg("check").arg("--no-default-features");
    if !features.is_empty() {
        command.arg("--features").arg(features.join(","));
    }
    if let Some(target) = target {
        command.arg("--target").arg(target);
    }
    command
}

fn cargo(repository: &Path) -> Command {
    let executable = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(executable);
    command.current_dir(repository);
    command
}

fn run_positive(description: &str, command: &mut Command) -> Result<(), String> {
    println!("CHECK {description}");
    let status = command
        .status()
        .map_err(|error| format!("could not run {command:?}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{description} failed with {status}"))
    }
}

fn run_expected_failure(description: &str, command: &mut Command) -> Result<(), String> {
    println!("CHECK expected rejection: {description}");
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = command
        .output()
        .map_err(|error| format!("could not run {command:?}: {error}"))?;
    if output.status.success() {
        Err(format!(
            "invalid configuration `{description}` unexpectedly compiled successfully"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_the_features_table() {
        let manifest = r#"
[package]
name = "sample"

[features]
default = []
port-test = ["std"] # comment
multi-line = [
    "port-test",
]

[dependencies]
dependency = "1"
"#;
        let parsed = parse_features(manifest);
        assert_eq!(
            parsed,
            ["default", "multi-line", "port-test"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    }
}
