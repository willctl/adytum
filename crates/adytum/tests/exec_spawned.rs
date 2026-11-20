//! The handed-back child: its pid, live stderr, a non-blocking exit check
//! and a group kill, under the same rules as `SystemRunner`.
#![cfg(unix)]
#![expect(clippy::expect_used, reason = "test helpers fail where they stand, as a test does")]

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use adytum::exec::{Command, Spawned};
use adytum::Error;

const SAFE_PATH: &str = "/usr/bin:/bin";

fn gone(pid: i32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid), None) == Err(Errno::ESRCH)
}

fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < end, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn sh(script: &str) -> Command {
    Command::new("/bin/sh").arg("-c").arg(script)
}

#[test]
fn the_pid_is_the_childs_and_leads_its_own_group() -> Result<(), Error> {
    let mut s = Spawned::start(&sh("echo $$; ps -o pgid= -p $$").env("PATH", SAFE_PATH))?;
    let mut out = String::new();
    if let Some(mut o) = s.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    let mut lines = out.lines().map(str::trim);
    assert_eq!(lines.next().and_then(|l| l.parse::<u32>().ok()), Some(s.pid()), "{out}");
    assert_eq!(lines.next().and_then(|l| l.parse::<u32>().ok()), Some(s.pid()), "its own process group: {out}");
    until("it exited", || s.try_wait().is_ok_and(|e| e.is_some()));
    Ok(())
}

#[test]
fn the_environment_is_empty_except_the_named_variables() -> Result<(), Error> {
    // The test process has a PATH and a HOME; neither may arrive.
    let mut s = Spawned::start(&Command::new("/usr/bin/env").env("ONLY_THIS", "1"))?;
    let mut out = String::new();
    if let Some(mut o) = s.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    assert_eq!(out, "ONLY_THIS=1\n");
    Ok(())
}

#[test]
fn a_missing_program_is_refused_by_name() {
    let err = Spawned::start(&Command::new("adytum-exec-no-such-program").env("PATH", SAFE_PATH)).expect_err("not on PATH");
    assert!(matches!(err, Error::ProgramNotFound { .. }), "{err}");
}

#[test]
fn stderr_is_readable_while_the_child_runs_and_keeps_only_the_tail() -> Result<(), Error> {
    let mut s = Spawned::start(
        &sh("echo early >&2; head -c 5000 /dev/zero | tr '\\0' a >&2; echo late >&2; while [ ! -f \"$GO\" ]; do sleep 0.02; done")
            .env("PATH", SAFE_PATH)
            .env("GO", "/nonexistent-adytum-go"),
    )?;
    let tail = s.stderr_tail(100).expect("stderr was a pipe");
    assert!(s.stderr_tail(100).is_none(), "stderr is taken once");
    until("stderr arrived", || tail.text().ends_with("late"));
    assert!(s.try_wait()?.is_none(), "the child is still running");
    let text = tail.text();
    assert_eq!(tail.bytes().len(), 100, "only the last bytes are kept");
    assert!(!text.contains("early"));
    s.kill_group();
    Ok(())
}

#[test]
fn try_wait_does_not_block_and_reports_the_exit() -> Result<(), Error> {
    let dir = std::env::temp_dir().join(format!("adytum-exec-spawned-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io { what: "make a folder", source })?;
    let go = dir.join("go");
    let mut s = Spawned::start(&sh("while [ ! -f \"$GO\" ]; do sleep 0.02; done; exit 3").env("PATH", SAFE_PATH).env("GO", &go))?;
    assert_eq!(s.try_wait()?, None);
    std::fs::write(&go, "").map_err(|source| Error::Io { what: "write", source })?;
    let mut exit = None;
    until("it exited", || {
        exit = s.try_wait().ok().flatten();
        exit.is_some()
    });
    let exit = exit.expect("exited");
    assert_eq!((exit.code, exit.timed_out), (Some(3), false));
    assert_eq!(s.try_wait()?, Some(exit), "asking again gives the same answer");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn stdin_reaches_the_child_and_closing_it_ends_it() -> Result<(), Error> {
    let mut s = Spawned::start(&Command::new("/bin/cat"))?;
    let mut stdin = s.stdin.take().expect("stdin pipe");
    stdin.write_all(b"hello\n").map_err(|source| Error::Io { what: "write", source })?;
    drop(stdin);
    let mut out = String::new();
    if let Some(mut o) = s.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    assert_eq!(out, "hello\n");
    until("it exited", || s.try_wait().is_ok_and(|e| e.is_some()));
    Ok(())
}

/// Starts a shell that runs a long sleep in the background and prints its
/// pid, so the sleep is a grandchild in the shell's group.
fn with_grandchild(extra: &str) -> Result<(Spawned, i32), Error> {
    let mut s = Spawned::start(&sh(&format!("sleep 600 & echo $!; {extra}")).env("PATH", SAFE_PATH))?;
    let mut line = String::new();
    let mut byte = [0u8; 1];
    let out = s.stdout.as_mut().expect("stdout pipe");
    while out.read(&mut byte).is_ok_and(|n| n == 1) && byte[0] != b'\n' {
        line.push(char::from(byte[0]));
    }
    let grandchild = line.trim().parse::<i32>().expect("a pid");
    assert!(!gone(grandchild));
    Ok((s, grandchild))
}

#[test]
fn killing_the_group_kills_a_grandchild() -> Result<(), Error> {
    let (mut s, grandchild) = with_grandchild("wait")?;
    assert!(s.group_running());
    s.kill_group();
    until("the grandchild died with the group", || gone(grandchild));
    assert!(!s.group_running());
    assert_eq!(s.try_wait()?.map(|e| e.code), Some(None), "ended by a signal");
    Ok(())
}

#[test]
fn the_group_stays_running_after_the_leader_exits_until_its_children_end() -> Result<(), Error> {
    let (mut s, grandchild) = with_grandchild("exit 0")?;
    until("the leader exited", || s.try_wait().is_ok_and(|e| e.is_some()));
    assert!(s.group_running(), "the sleep is still in the group");
    s.kill_group();
    until("the grandchild died", || gone(grandchild));
    assert!(!s.group_running());
    Ok(())
}

#[test]
fn the_deadline_kills_the_group_when_it_is_checked() -> Result<(), Error> {
    let mut s = Spawned::start(&sh("sleep 600 & echo $!; wait").env("PATH", SAFE_PATH).deadline(Duration::from_millis(200)))?;
    let mut line = String::new();
    let mut byte = [0u8; 1];
    let out = s.stdout.as_mut().expect("stdout pipe");
    while out.read(&mut byte).is_ok_and(|n| n == 1) && byte[0] != b'\n' {
        line.push(char::from(byte[0]));
    }
    let grandchild = line.trim().parse::<i32>().expect("a pid");
    let mut exit = None;
    until("the deadline stopped it", || {
        exit = s.try_wait().ok().flatten();
        exit.is_some()
    });
    assert_eq!(exit.map(|e| (e.code, e.timed_out)), Some((None, true)));
    until("the grandchild died", || gone(grandchild));
    Ok(())
}

#[test]
fn dropping_a_running_child_kills_its_group() -> Result<(), Error> {
    let (s, grandchild) = with_grandchild("wait")?;
    drop(s);
    until("the grandchild died", || gone(grandchild));
    Ok(())
}

#[test]
fn the_deadline_also_covers_what_outlives_the_leader() -> Result<(), Error> {
    let mut s = Spawned::start(&sh("sleep 600 & echo $!; exit 0").env("PATH", SAFE_PATH).deadline(Duration::from_millis(300)))?;
    let mut line = String::new();
    let mut byte = [0u8; 1];
    let out = s.stdout.as_mut().expect("stdout pipe");
    while out.read(&mut byte).is_ok_and(|n| n == 1) && byte[0] != b'\n' {
        line.push(char::from(byte[0]));
    }
    let grandchild = line.trim().parse::<i32>().expect("a pid");
    let mut exit = None;
    until("the group ended", || {
        exit = s.try_wait().ok().flatten();
        exit.is_some() && !s.group_running()
    });
    assert_eq!(exit.map(|e| (e.code, e.timed_out)), Some((Some(0), true)), "the leader's own code, and the deadline stopped the rest");
    until("the grandchild died", || gone(grandchild));
    Ok(())
}
