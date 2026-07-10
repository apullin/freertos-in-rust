use std::collections::BTreeSet;

#[derive(Debug, Eq, PartialEq)]
pub struct TapReport {
    pub test_count: usize,
}

pub fn parse(stdout: &[u8], stderr: &[u8]) -> Result<TapReport, Vec<String>> {
    let mut parser = Parser::default();
    parser.consume(stdout, "stdout");
    parser.consume(stderr, "stderr");
    parser.finish()
}

#[derive(Default)]
struct Parser {
    saw_version: bool,
    plan: Option<usize>,
    numbers: BTreeSet<usize>,
    result_count: usize,
    sentinel_count: usize,
    sentinel_passed: bool,
    errors: Vec<String>,
}

impl Parser {
    fn consume(&mut self, bytes: &[u8], stream: &str) {
        let text = String::from_utf8_lossy(bytes);
        for raw_line in text.lines() {
            let line = raw_line.trim().trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }

            if line == "TAP version 13" {
                if self.saw_version {
                    self.errors
                        .push("duplicate `TAP version 13` header".to_owned());
                }
                self.saw_version = true;
                continue;
            }

            if let Some(value) = line.strip_prefix("FREERUSTOS_TEST_RESULT=") {
                self.sentinel_count += 1;
                match value {
                    "PASS" => self.sentinel_passed = true,
                    "FAIL" => self
                        .errors
                        .push(format!("guest reported failure on {stream}")),
                    _ => self.errors.push(format!(
                        "invalid result sentinel value {value:?} on {stream}"
                    )),
                }
                continue;
            }

            if let Some(count) = parse_plan(line) {
                match count {
                    Ok(count) if self.plan.is_none() => self.plan = Some(count),
                    Ok(_) => self.errors.push("duplicate TAP plan".to_owned()),
                    Err(error) => self.errors.push(error),
                }
                continue;
            }

            if line.starts_with("ok ") || line.starts_with("not ok ") {
                self.consume_result(line);
                continue;
            }

            let lower = line.to_ascii_lowercase();
            if lower.contains("panicked at")
                || lower.contains("panic:")
                || lower.contains("assertion failed")
                || lower.contains("assert failed")
                || lower.contains("hardfault")
            {
                self.errors
                    .push(format!("guest panic/assertion text on {stream}: {line}"));
            }
        }
    }

    fn consume_result(&mut self, line: &str) {
        let (passed, remainder) = if let Some(remainder) = line.strip_prefix("ok ") {
            (true, remainder)
        } else {
            (false, line.strip_prefix("not ok ").unwrap_or_default())
        };
        let number_text = remainder
            .split_once(|character: char| character.is_ascii_whitespace() || character == '-')
            .map_or(remainder, |(number, _)| number);

        match number_text.parse::<usize>() {
            Ok(0) => self
                .errors
                .push(format!("invalid TAP test number in {line:?}")),
            Ok(number) => {
                self.result_count += 1;
                if !self.numbers.insert(number) {
                    self.errors
                        .push(format!("duplicate TAP test number {number}"));
                }
                if !passed {
                    self.errors.push(format!("guest test failed: {line}"));
                }
            }
            Err(_) => self
                .errors
                .push(format!("invalid TAP result line {line:?}")),
        }
    }

    fn finish(mut self) -> Result<TapReport, Vec<String>> {
        if !self.saw_version {
            self.errors
                .push("missing `TAP version 13` header".to_owned());
        }
        if self.sentinel_count == 0 {
            self.errors
                .push("missing final `FREERUSTOS_TEST_RESULT` sentinel".to_owned());
        } else if self.sentinel_count > 1 {
            self.errors.push(format!(
                "duplicate result sentinel (saw {})",
                self.sentinel_count
            ));
        }
        if !self.sentinel_passed && self.sentinel_count != 0 {
            self.errors.push("guest did not report PASS".to_owned());
        }

        match self.plan {
            None => self.errors.push("missing TAP plan (`1..N`)".to_owned()),
            Some(count) => {
                if count == 0 {
                    self.errors.push("TAP plan contains zero tests".to_owned());
                }
                if self.result_count != count {
                    self.errors.push(format!(
                        "TAP plan expected {count} results, but saw {}",
                        self.result_count
                    ));
                }
                for number in 1..=count {
                    if !self.numbers.contains(&number) {
                        self.errors
                            .push(format!("missing TAP test number {number}"));
                    }
                }
                if let Some(number) = self.numbers.iter().find(|number| **number > count) {
                    self.errors.push(format!(
                        "TAP test number {number} exceeds declared plan 1..{count}"
                    ));
                }
            }
        }

        if self.errors.is_empty() {
            Ok(TapReport {
                test_count: self.result_count,
            })
        } else {
            Err(self.errors)
        }
    }
}

fn parse_plan(line: &str) -> Option<Result<usize, String>> {
    let count = line.strip_prefix("1..")?;
    Some(
        count
            .parse::<usize>()
            .map_err(|_| format!("invalid TAP plan {line:?}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &[u8] = b"boot noise\nTAP version 13\nok 1 - first\nok 2 - second\n1..2\nFREERUSTOS_TEST_RESULT=PASS\n";

    #[test]
    fn accepts_complete_tap() {
        assert_eq!(parse(VALID, b"qemu noise"), Ok(TapReport { test_count: 2 }));
    }

    #[test]
    fn rejects_not_ok_even_with_pass_sentinel() {
        let output = b"TAP version 13\nnot ok 1 - broken\n1..1\nFREERUSTOS_TEST_RESULT=PASS\n";
        let errors = parse(output, b"").unwrap_err().join("\n");
        assert!(errors.contains("guest test failed"));
    }

    #[test]
    fn rejects_duplicate_and_missing_numbers() {
        let output =
            b"TAP version 13\nok 1 - first\nok 1 - duplicate\n1..2\nFREERUSTOS_TEST_RESULT=PASS\n";
        let errors = parse(output, b"").unwrap_err().join("\n");
        assert!(errors.contains("duplicate TAP test number 1"));
        assert!(errors.contains("missing TAP test number 2"));
    }

    #[test]
    fn rejects_missing_or_duplicate_sentinel() {
        let missing = b"TAP version 13\nok 1 - first\n1..1\n";
        assert!(parse(missing, b"")
            .unwrap_err()
            .iter()
            .any(|error| error.contains("missing final")));

        let duplicate = b"TAP version 13\nok 1 - first\n1..1\nFREERUSTOS_TEST_RESULT=PASS\nFREERUSTOS_TEST_RESULT=PASS\n";
        assert!(parse(duplicate, b"")
            .unwrap_err()
            .iter()
            .any(|error| error.contains("duplicate result")));
    }

    #[test]
    fn rejects_panics_on_stderr() {
        let errors = parse(VALID, b"thread panicked at src/main.rs\n")
            .unwrap_err()
            .join("\n");
        assert!(errors.contains("panic/assertion"));
    }
}
