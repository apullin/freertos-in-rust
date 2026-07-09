//! Small, target-independent helpers shared by the QEMU regression firmware.

#![no_std]

use core::fmt::{self, Write};

/// A minimal TAP producer suitable for a `no_std` guest.
///
/// The host-side runner treats any `not ok` line, an invalid plan, or a final
/// failure sentinel as a failed firmware test.
pub struct Tap<W: Write> {
    output: W,
    next_test: usize,
    failures: usize,
    output_failed: bool,
}

impl<W: Write> Tap<W> {
    /// Start a TAP stream.
    pub fn new(mut output: W) -> Self {
        let output_failed = writeln!(output, "TAP version 13").is_err();

        Self {
            output,
            next_test: 1,
            failures: 0,
            output_failed,
        }
    }

    /// Record one numbered assertion.
    pub fn check(&mut self, passed: bool, name: &str) {
        let status = if passed { "ok" } else { "not ok" };

        if writeln!(self.output, "{} {} - {}", status, self.next_test, name).is_err() {
            self.output_failed = true;
        }

        if !passed {
            self.failures += 1;
        }

        self.next_test += 1;
    }

    /// Emit a diagnostic which does not consume a test number.
    pub fn diagnostic(&mut self, arguments: fmt::Arguments<'_>) {
        if write!(self.output, "# ").is_err()
            || self.output.write_fmt(arguments).is_err()
            || writeln!(self.output).is_err()
        {
            self.output_failed = true;
        }
    }

    /// Close the TAP plan and emit the machine-readable result sentinel.
    pub fn finish(&mut self, planned_tests: usize) -> bool {
        let actual_tests = self.next_test - 1;
        let plan_matches = actual_tests == planned_tests;

        if !plan_matches {
            self.failures += 1;
            self.diagnostic(format_args!(
                "plan mismatch: expected {} tests, emitted {}",
                planned_tests, actual_tests
            ));
        }

        if writeln!(self.output, "1..{}", planned_tests).is_err() {
            self.output_failed = true;
        }

        let passed = self.failures == 0 && !self.output_failed;
        let result = if passed { "PASS" } else { "FAIL" };

        if writeln!(self.output, "FREERUSTOS_TEST_RESULT={}", result).is_err() {
            self.output_failed = true;
        }

        passed && !self.output_failed
    }
}
