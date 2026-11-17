//! Runs a helper tool (pactl, dbus-send, osascript, ps) with a hard timeout
//! so a wedged audio server can never hang the media worker forever.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Stdout of a successful run, `None` on spawn failure, non-zero exit or
/// timeout (the child is killed).
pub(crate) fn run(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Drain on a side thread so a chatty child cannot block on a full pipe.
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text = reader.join().ok()?;
    status.filter(|status| status.success()).map(|_| text)
}
