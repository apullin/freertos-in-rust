use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

const RESULT_PREFIX: &[u8] = b"FREERUSTOS_TEST_RESULT=";

#[derive(Debug)]
pub struct CapturedProcess {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub terminated_after_result: bool,
    pub elapsed: Duration,
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

enum Event {
    Bytes(Stream, Vec<u8>),
    Done,
}

pub fn run_captured(
    command: &mut Command,
    timeout: Duration,
    stop_after_result: bool,
) -> Result<CapturedProcess, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let command_description = format!("{command:?}");
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {command_description}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture child stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture child stderr".to_owned())?;

    let (sender, receiver) = mpsc::channel();
    let stdout_reader = spawn_reader(stdout, Stream::Stdout, sender.clone());
    let stderr_reader = spawn_reader(stderr, Stream::Stderr, sender);
    let started = Instant::now();
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut done_readers = 0;
    let mut timed_out = false;
    let mut terminated_after_result = false;
    let mut result_seen = false;
    let status;

    loop {
        receive_available(
            &receiver,
            &mut stdout_bytes,
            &mut stderr_bytes,
            &mut done_readers,
        );

        // Observe a natural exit before deciding to terminate on the sentinel.
        // This preserves a non-zero QEMU exit as a failure even if its final
        // output happened to contain a result line.
        if let Some(exit_status) = child
            .try_wait()
            .map_err(|error| format!("could not poll {command_description}: {error}"))?
        {
            status = exit_status;
            break;
        }

        if stop_after_result
            && !result_seen
            && (contains(&stdout_bytes, RESULT_PREFIX) || contains(&stderr_bytes, RESULT_PREFIX))
        {
            result_seen = true;
            terminated_after_result = true;
            child.kill().map_err(|error| {
                format!("could not stop {command_description} after its result: {error}")
            })?;
        }

        if started.elapsed() >= timeout {
            timed_out = true;
            child.kill().map_err(|error| {
                format!("could not terminate timed-out {command_description}: {error}")
            })?;
            status = child
                .wait()
                .map_err(|error| format!("could not reap {command_description}: {error}"))?;
            break;
        }

        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(event) => handle_event(
                event,
                &mut stdout_bytes,
                &mut stderr_bytes,
                &mut done_readers,
            ),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {}
        }
    }

    while done_readers < 2 {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => handle_event(
                event,
                &mut stdout_bytes,
                &mut stderr_bytes,
                &mut done_readers,
            ),
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    stdout_reader
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned())??;
    stderr_reader
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned())??;

    Ok(CapturedProcess {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        timed_out,
        terminated_after_result,
        elapsed: started.elapsed(),
    })
}

fn spawn_reader<R>(
    mut reader: R,
    stream: Stream,
    sender: Sender<Event>,
) -> thread::JoinHandle<Result<(), String>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| format!("failed to read child output: {error}"))?;
            if count == 0 {
                break;
            }
            if sender
                .send(Event::Bytes(stream, buffer[..count].to_vec()))
                .is_err()
            {
                return Ok(());
            }
        }
        let _ = sender.send(Event::Done);
        Ok(())
    })
}

fn receive_available(
    receiver: &Receiver<Event>,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    done_readers: &mut usize,
) {
    while let Ok(event) = receiver.try_recv() {
        handle_event(event, stdout, stderr, done_readers);
    }
}

fn handle_event(
    event: Event,
    stdout: &mut Vec<u8>,
    stderr: &mut Vec<u8>,
    done_readers: &mut usize,
) {
    match event {
        Event::Bytes(Stream::Stdout, bytes) => stdout.extend_from_slice(&bytes),
        Event::Bytes(Stream::Stderr, bytes) => stderr.extend_from_slice(&bytes),
        Event::Done => *done_readers += 1,
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn captures_both_output_streams() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf stdout; printf stderr >&2");
        let output = run_captured(&mut command, Duration::from_secs(2), false).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
        assert!(!output.timed_out);
    }

    #[cfg(unix)]
    #[test]
    fn terminates_a_process_at_its_result_without_waiting_for_eof() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "printf 'TAP version 13\\nok 1 - sample\\n1..1\\nFREERUSTOS_TEST_RESULT=PASS\\n'; sleep 5",
        );
        let output = run_captured(&mut command, Duration::from_secs(2), true).unwrap();
        assert!(output.terminated_after_result);
        assert!(!output.timed_out);
        assert!(output.elapsed < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn terminates_a_timed_out_process() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("exec sleep 5");
        let output = run_captured(&mut command, Duration::from_millis(75), false).unwrap();
        assert!(output.timed_out);
        assert!(output.elapsed < Duration::from_secs(2));
    }
}
