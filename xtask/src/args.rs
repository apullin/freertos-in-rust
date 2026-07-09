use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Help,
    TestHost,
    CheckFeatures,
    QemuTest(QemuTestArgs),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QemuTestArgs {
    pub arch: ArchitectureSelection,
    pub suite: String,
    pub cpus: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchitectureSelection {
    One(Architecture),
    All,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Architecture {
    CortexM0,
    CortexM3,
    CortexM4f,
    CortexM7,
    Riscv32,
    CortexA9,
    CortexA53,
    CortexA9Smp,
}

impl Architecture {
    pub const SINGLE_CORE: [Self; 7] = [
        Self::CortexM0,
        Self::CortexM3,
        Self::CortexM4f,
        Self::CortexM7,
        Self::Riscv32,
        Self::CortexA9,
        Self::CortexA53,
    ];

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "cortex-m0" => Ok(Self::CortexM0),
            "cortex-m3" => Ok(Self::CortexM3),
            "cortex-m4f" => Ok(Self::CortexM4f),
            "cortex-m7" => Ok(Self::CortexM7),
            "riscv32" => Ok(Self::Riscv32),
            "cortex-a9" => Ok(Self::CortexA9),
            "cortex-a53" => Ok(Self::CortexA53),
            "cortex-a9-smp" => Ok(Self::CortexA9Smp),
            _ => Err(format!(
                "unknown architecture {value:?}; expected one of: {}",
                Self::names().join(", ")
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CortexM0 => "cortex-m0",
            Self::CortexM3 => "cortex-m3",
            Self::CortexM4f => "cortex-m4f",
            Self::CortexM7 => "cortex-m7",
            Self::Riscv32 => "riscv32",
            Self::CortexA9 => "cortex-a9",
            Self::CortexA53 => "cortex-a53",
            Self::CortexA9Smp => "cortex-a9-smp",
        }
    }

    fn names() -> Vec<&'static str> {
        let mut names: Vec<_> = Self::SINGLE_CORE.iter().map(|arch| arch.as_str()).collect();
        names.push(Self::CortexA9Smp.as_str());
        names
    }
}

impl fmt::Display for Architecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn parse<I, S>(arguments: I) -> Result<Action, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();

    match arguments.as_slice() {
        [] => Err("missing command (try `xtask --help`)".to_owned()),
        [argument] if argument == "-h" || argument == "--help" || argument == "help" => {
            Ok(Action::Help)
        }
        [command, target] if command == "test" && target == "host" => Ok(Action::TestHost),
        [command] if command == "check-features" => Ok(Action::CheckFeatures),
        [command, rest @ ..] if command == "qemu-test" => parse_qemu_test(rest),
        [command, ..] => Err(format!(
            "unknown or malformed command {command:?} (try `xtask --help`)"
        )),
    }
}

fn parse_qemu_test(arguments: &[String]) -> Result<Action, String> {
    let mut arch = None;
    let mut suite = None;
    let mut cpus = None;
    let mut index = 0;

    while index < arguments.len() {
        let option = arguments[index].as_str();
        index += 1;
        let value = arguments
            .get(index)
            .ok_or_else(|| format!("{option} requires a value"))?;
        index += 1;

        match option {
            "--arch" => {
                if arch.is_some() {
                    return Err("--arch was supplied more than once".to_owned());
                }
                arch = Some(if value == "all" {
                    ArchitectureSelection::All
                } else {
                    ArchitectureSelection::One(Architecture::parse(value)?)
                });
            }
            "--suite" => {
                if suite.is_some() {
                    return Err("--suite was supplied more than once".to_owned());
                }
                validate_suite(value)?;
                suite = Some(value.clone());
            }
            "--cpus" => {
                if cpus.is_some() {
                    return Err("--cpus was supplied more than once".to_owned());
                }
                let parsed = value.parse::<usize>().map_err(|_| {
                    format!("invalid CPU count {value:?}; expected a positive integer")
                })?;
                if parsed == 0 {
                    return Err("--cpus must be greater than zero".to_owned());
                }
                cpus = Some(parsed);
            }
            _ => return Err(format!("unknown qemu-test option {option:?}")),
        }
    }

    let arch = arch.ok_or_else(|| "qemu-test requires --arch <architecture>".to_owned())?;
    let suite = suite.ok_or_else(|| "qemu-test requires --suite <suite>".to_owned())?;

    if arch == ArchitectureSelection::All && cpus.unwrap_or(1) != 1 {
        return Err("--cpus greater than one cannot be combined with --arch all".to_owned());
    }

    Ok(Action::QemuTest(QemuTestArgs { arch, suite, cpus }))
}

fn validate_suite(suite: &str) -> Result<(), String> {
    if suite.is_empty()
        || suite
            .bytes()
            .any(|byte| !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
    {
        return Err(format!(
            "invalid suite {suite:?}; use lowercase ASCII letters, digits, and hyphens"
        ));
    }
    Ok(())
}

pub fn usage() -> &'static str {
    "FreeRusTOS project test driver

Usage:
  cargo run --manifest-path xtask/Cargo.toml -- test host
  cargo run --manifest-path xtask/Cargo.toml -- check-features
  cargo run --manifest-path xtask/Cargo.toml -- qemu-test --arch <ARCH> --suite <SUITE> [--cpus N]

Architectures:
  cortex-m0, cortex-m3, cortex-m4f, cortex-m7, riscv32, cortex-a9,
  cortex-a53, cortex-a9-smp, or all (all single-core ports)

Suites:
  A suite is the matching binary name in tests/qemu/<ARCH>/Cargo.toml.
  `all` runs every standard suite applicable to the selected architecture.

Environment:
  FREERUSTOS_QEMU_TIMEOUT_SECS  Per-firmware timeout (default: 25)
"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_top_level_command() {
        assert_eq!(parse(["test", "host"]), Ok(Action::TestHost));
        assert_eq!(parse(["check-features"]), Ok(Action::CheckFeatures));
        assert_eq!(parse(["--help"]), Ok(Action::Help));
    }

    #[test]
    fn parses_qemu_options_in_any_order() {
        let parsed = parse([
            "qemu-test",
            "--suite",
            "kernel-core",
            "--cpus",
            "2",
            "--arch",
            "cortex-a9-smp",
        ]);
        assert_eq!(
            parsed,
            Ok(Action::QemuTest(QemuTestArgs {
                arch: ArchitectureSelection::One(Architecture::CortexA9Smp),
                suite: "kernel-core".to_owned(),
                cpus: Some(2),
            }))
        );
    }

    #[test]
    fn rejects_missing_duplicate_and_invalid_options() {
        assert!(parse(["qemu-test", "--arch", "cortex-m4f"]).is_err());
        assert!(parse([
            "qemu-test",
            "--arch",
            "cortex-m4f",
            "--arch",
            "cortex-m3",
            "--suite",
            "smoke",
        ])
        .is_err());
        assert!(parse(["qemu-test", "--arch", "cortex-m4f", "--suite", "../smoke",]).is_err());
        assert!(parse([
            "qemu-test",
            "--arch",
            "all",
            "--suite",
            "smoke",
            "--cpus",
            "2",
        ])
        .is_err());
    }
}
