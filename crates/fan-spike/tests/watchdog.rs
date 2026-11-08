//! Runs the real watchdog process without root. Its Auto write is refused or a no-op here,
//! so these cover when it decides to act, not whether the fan changes.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn spawn_ready() -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fan-spike"))
        .args(["watchdog", "0"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "ready");
    child
}

fn exits_within(child: &mut Child, limit: Duration) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if child.try_wait().unwrap().is_some() {
            return Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[test]
fn acts_when_the_parent_closes_the_pipe() {
    let mut child = spawn_ready();
    drop(child.stdin.take());
    assert!(exits_within(&mut child, Duration::from_secs(3)).is_some());
}

#[test]
fn acts_when_heartbeats_stop_with_the_pipe_open() {
    let mut child = spawn_ready();
    let _held_open = child.stdin.take();
    let elapsed = exits_within(&mut child, Duration::from_secs(8)).expect("should act");
    assert!(elapsed >= Duration::from_millis(2500), "acted early: {elapsed:?}");
}

#[test]
fn waits_while_heartbeats_continue() {
    let mut child = spawn_ready();
    let mut beat = child.stdin.take().unwrap();
    for _ in 0..10 {
        beat.write_all(b".").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(child.try_wait().unwrap().is_none(), "acted despite heartbeats");
    }
    drop(beat);
    assert!(exits_within(&mut child, Duration::from_secs(3)).is_some());
}
