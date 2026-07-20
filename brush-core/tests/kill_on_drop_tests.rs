//! Integration tests for the opt-in `kill_external_commands_on_drop` creation
//! option.
//!
//! By default a spawned child outlives the shell that spawned it. That is the
//! only correct behavior for a real shell — job control, disowned jobs, and
//! `nohup`-style usage all depend on a child surviving its parent — and these
//! tests pin it as the default.
//!
//! An *embedded* shell has the opposite requirement. There the shell is an
//! object the host creates and destroys, so a child that survives teardown is a
//! leak: it keeps running unattended, and it holds a duplicate of the shell's
//! stdout/stderr pipe, so a host draining that output never sees EOF.
//!
//! # What owns a spawned child
//!
//! The tests tear down the whole runtime rather than just dropping the `Shell`,
//! because that is what actually reaches a running child. A backgrounded
//! command (`cmd &`) is executed by a detached `tokio` task operating on a
//! *clone* of the shell, and the resulting `ChildProcess` is owned by that task
//! — not by the spawning shell's job table, which only holds the task's
//! `JoinHandle`. Dropping a `JoinHandle` detaches rather than aborts, so
//! dropping the parent shell alone leaves such a child untouched. Shutting the
//! runtime down drops the tasks, which drops the `Child`, which is where
//! `kill_on_drop` takes effect.

#![cfg(unix)]
#![cfg(test)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_used
)]

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;

/// How long to wait for a killed process to actually disappear.
const REAP_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for the child to report its pid.
const PID_TIMEOUT: Duration = Duration::from_secs(10);

/// Returns whether a process is still running.
///
/// A killed-but-unreaped process lingers as a zombie, and `kill -0` reports
/// success for one, so this inspects the process *state* and treats `Z` as not
/// running.
fn process_is_running(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "state=", "-p", pid.to_string().as_str()])
        .output()
        .expect("failed to run `ps`");

    let state = String::from_utf8_lossy(&output.stdout);
    let state = state.trim();

    !state.is_empty() && !state.starts_with('Z')
}

/// Waits until `pid` is no longer running, returning whether it went away
/// within [`REAP_TIMEOUT`].
fn wait_for_exit(pid: u32) -> bool {
    let deadline = Instant::now() + REAP_TIMEOUT;
    while Instant::now() < deadline {
        if !process_is_running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !process_is_running(pid)
}

/// Reads the pid the child wrote to `path`, waiting for the write to land.
fn read_pid(path: &Path) -> Result<u32> {
    let deadline = Instant::now() + PID_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                return Ok(pid);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    anyhow::bail!("child never reported its pid at {}", path.display())
}

/// Builds a shell with the option set as requested, starts a long-running
/// child, tears the whole runtime down, and returns the child's pid so the
/// caller can check whether it survived.
fn spawn_child_then_tear_down(kill_on_drop: bool, pid_file: &Path) -> Result<u32> {
    // A multi-threaded runtime is required: the backgrounded command runs on a
    // separate task, and the pid poll below blocks a worker.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_io()
        .build()?;

    // The child reports its own pid rather than relying on `$!`, and `exec`s
    // into `sleep`, so the pid it wrote is the process the shell holds a
    // handle to.
    let script = std::format!(
        "export PATH=/bin:/usr/bin; sh -c 'echo $$ > {}; exec sleep 300' &",
        pid_file.display()
    );

    let pid = runtime.block_on(async {
        let builtins = brush_builtins::default_builtins::<
            brush_core::extensions::DefaultShellExtensions,
        >(brush_builtins::BuiltinSet::BashMode);

        let mut shell = brush_core::Shell::builder()
            .builtins(builtins)
            .do_not_inherit_env(true)
            .skip_well_known_vars(true)
            .kill_external_commands_on_drop(kill_on_drop)
            .build()
            .await?;

        let params = shell.default_exec_params();
        shell
            .run_string(script.as_str(), &brush_core::SourceInfo::default(), &params)
            .await?;

        read_pid(pid_file)
    })?;

    assert!(
        process_is_running(pid),
        "child {pid} should be running before teardown"
    );

    // Tear everything down: this drops the task that owns the child, and with
    // it the `Child` handle that carries the kill-on-drop request.
    drop(runtime);

    Ok(pid)
}

/// With the option ON, tearing the shell down reaps the child it spawned.
#[test]
fn child_is_killed_on_teardown_with_option_on() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let pid_file = dir.path().join("pid");

    let pid = spawn_child_then_tear_down(true, pid_file.as_path())?;

    assert!(
        wait_for_exit(pid),
        "with kill_external_commands_on_drop enabled, child {pid} should have been \
         killed on teardown, but it is still running"
    );

    Ok(())
}

/// With the option OFF (the default), the child outlives teardown, exactly as
/// it does today. This is the behavior real shells depend on.
#[test]
fn child_outlives_teardown_by_default() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let pid_file = dir.path().join("pid");

    let pid = spawn_child_then_tear_down(false, pid_file.as_path())?;

    // Give the child every chance to die, so a regression to
    // kill-on-drop-by-default is caught rather than raced past.
    std::thread::sleep(Duration::from_millis(500));
    let survived = process_is_running(pid);

    // Clean up regardless of the assertion's outcome; this test intentionally
    // leaves a live process behind if it does not.
    let _ = std::process::Command::new("kill")
        .args(["-9", pid.to_string().as_str()])
        .status();

    assert!(
        survived,
        "by default a spawned child must outlive its shell (job control and \
         `nohup`-style usage depend on it), but child {pid} was killed"
    );

    Ok(())
}

/// The option must default to off, so merely adding it changes nothing for
/// existing consumers.
#[tokio::test]
async fn option_defaults_to_disabled() -> Result<()> {
    let shell = brush_core::Shell::builder()
        .do_not_inherit_env(true)
        .skip_well_known_vars(true)
        .build()
        .await?;

    assert!(
        !shell.options().kill_external_commands_on_drop,
        "kill_external_commands_on_drop must default to disabled"
    );

    Ok(())
}
