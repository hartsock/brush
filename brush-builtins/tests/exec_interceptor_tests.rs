//! Regression tests for the `exec` builtin's authorization.
//!
//! `exec` outside a subshell replaces the shell process image. It composes its
//! `std::process::Command` directly and calls `CommandExt::exec`, so it never reaches
//! `commands::execute_external_command` and is not covered by that path's authorization.
//! If the builtin fails to authorize, the process is *replaced* — a broken implementation
//! would silently run the denied program and destroy the test runner along with it.
//!
//! Every case therefore runs in a dedicated child process: this same test binary, re-invoked
//! with `CHILD_DIR_ENV` set, running only `exec_interceptor_child`. The child records what
//! the interceptor saw and exits with the shell's status; the parent asserts on the record,
//! the exit status, and the absence of the side effect the denied program would have had. If
//! authorization regresses, the child is replaced by `touch`, the marker appears, and the
//! parent fails.
#![cfg(all(unix, feature = "builtin.exec"))]
#![cfg(test)]
#![allow(clippy::panic_in_result_fn)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use brush_core::extensions::{CommandInterceptor, ExecDecision, ExecRequest};

/// Set on the child to the scratch directory it should work in.
const CHILD_DIR_ENV: &str = "BRUSH_EXEC_INTERCEPTOR_CHILD_DIR";
/// Set on the child to the shell script it should run.
const CHILD_SCRIPT_ENV: &str = "BRUSH_EXEC_INTERCEPTOR_CHILD_SCRIPT";
/// Placeholder in a test script for the marker path the denied program would create.
const MARKER: &str = "@MARKER@";

/// Denies everything, and appends one tab-separated line per consultation to `record`.
struct RecordingDenier {
    record: PathBuf,
    seen: Mutex<usize>,
}

impl CommandInterceptor for RecordingDenier {
    fn before_exec(&self, request: &ExecRequest<'_>) -> ExecDecision {
        *self.seen.lock().unwrap() += 1;
        let line = format!(
            "{}\t{}\t{}\t{}\n",
            request.command_name,
            request.program.display(),
            request.argv0.to_string_lossy(),
            request.args.join(" ")
        );
        // Append, so a second (unexpected) consultation is visible to the parent.
        let existing = std::fs::read_to_string(&self.record).unwrap_or_default();
        std::fs::write(&self.record, existing + &line).unwrap();
        ExecDecision::Deny("denied by test policy".to_string())
    }
}

/// The child. A no-op in an ordinary test run; does the work only when re-invoked with
/// `CHILD_DIR_ENV` set.
#[tokio::test]
async fn exec_interceptor_child() {
    let (Ok(dir), Ok(script)) = (
        std::env::var(CHILD_DIR_ENV),
        std::env::var(CHILD_SCRIPT_ENV),
    ) else {
        return;
    };
    let dir = PathBuf::from(dir);

    let policy = Arc::new(RecordingDenier {
        record: dir.join("record"),
        seen: Mutex::new(0),
    });

    let builtins = brush_builtins::default_builtins::<brush_core::extensions::DefaultShellExtensions>(
        brush_builtins::BuiltinSet::BashMode,
    );

    let mut shell = brush_core::Shell::builder()
        .command_interceptor(policy.clone() as Arc<dyn CommandInterceptor>)
        .builtins(builtins)
        .do_not_inherit_env(true)
        .skip_well_known_vars(true)
        .build()
        .await
        .unwrap();

    shell
        .set_env_global(
            "PATH",
            brush_core::variables::ShellVariable::new("/bin:/usr/bin"),
        )
        .unwrap();

    let params = shell.default_exec_params();
    let code = match shell
        .run_string(&script, &brush_core::SourceInfo::default(), &params)
        .await
    {
        Ok(result) => u8::from(result.exit_code),
        // A denial surfaces as an error out of the builtin; report what the shell would.
        Err(e) => u8::from(brush_core::ExecutionExitCode::from(&e)),
    };

    std::fs::write(dir.join("seen"), policy.seen.lock().unwrap().to_string()).unwrap();
    // Exit explicitly: reaching here at all is the point, and the status is an assertion.
    std::process::exit(i32::from(code));
}

struct ChildRun {
    exit_code: Option<i32>,
    consultations: usize,
    record: Vec<Vec<String>>,
    dir: tempfile::TempDir,
}

impl ChildRun {
    fn marker(&self) -> PathBuf {
        self.dir.path().join("marker")
    }
}

/// Runs `script` in a dedicated child process. The `MARKER` placeholder in the script is
/// replaced with a path inside the child's scratch directory that nothing else creates.
fn run_in_child(script: &str) -> Result<ChildRun> {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("marker");
    let script = script.replace(MARKER, &marker.display().to_string());

    let output = std::process::Command::new(std::env::current_exe()?)
        .args(["exec_interceptor_child", "--exact", "--test-threads=1"])
        .env(CHILD_DIR_ENV, dir.path())
        .env(CHILD_SCRIPT_ENV, &script)
        .output()?;

    let record = std::fs::read_to_string(dir.path().join("record"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.split('\t').map(str::to_string).collect())
        .collect();
    let consultations = std::fs::read_to_string(dir.path().join("seen"))
        .unwrap_or_default()
        .trim()
        .parse()
        .unwrap_or(0);

    Ok(ChildRun {
        exit_code: output.status.code(),
        consultations,
        record,
        dir,
    })
}

/// The load-bearing case. `exec` must consult the interceptor and refuse, rather than
/// replacing the process with the denied program.
#[test]
fn denied_exec_builtin_does_not_replace_the_process() -> Result<()> {
    let run = run_in_child("exec /usr/bin/touch @MARKER@")?;

    assert!(
        !run.marker().exists(),
        "the denied program ran and replaced the shell: {} exists",
        run.marker().display()
    );
    assert_eq!(
        run.exit_code,
        Some(126),
        "a denied exec should report 'cannot execute' (126); record: {:?}",
        run.record
    );
    assert_eq!(
        run.consultations, 1,
        "the interceptor should be consulted exactly once; saw {:?}",
        run.record
    );
    Ok(())
}

/// The request must distinguish the spelling entered from the executable resolved.
#[test]
fn exec_request_separates_command_name_from_resolved_program() -> Result<()> {
    let run = run_in_child("exec touch @MARKER@")?;

    assert!(!run.marker().exists());
    assert_eq!(run.record.len(), 1, "record: {:?}", run.record);
    let entry = &run.record[0];
    assert_eq!(entry[0], "touch", "command_name should be as written");
    // Which directory wins depends on the PATH order and the platform's layout, so assert
    // the contract -- resolved and absolute -- rather than one distribution's answer.
    assert!(
        Path::new(&entry[1]).is_absolute() && entry[1].ends_with("/touch"),
        "program should be the resolved absolute executable, got {:?}",
        entry[1]
    );
    assert_ne!(
        entry[0], entry[1],
        "command_name and program are distinct concepts and should not be conflated"
    );
    Ok(())
}

/// `exec -a NAME` overrides argv[0]; the interceptor must see the real zeroth argument, not
/// the program name.
#[test]
fn exec_argv0_override_is_visible_to_the_interceptor() -> Result<()> {
    let run = run_in_child("exec -a impostor /usr/bin/touch @MARKER@")?;

    assert!(!run.marker().exists());
    assert_eq!(run.record.len(), 1, "record: {:?}", run.record);
    let entry = &run.record[0];
    assert_eq!(entry[0], "/usr/bin/touch", "command_name");
    assert_eq!(entry[1], "/usr/bin/touch", "program");
    assert_eq!(entry[2], "impostor", "argv0 should be the -a override");
    Ok(())
}

/// `exec -l` prefixes argv[0] with `-`; that too is part of the argv0 contract.
#[test]
fn exec_login_dash_prefix_is_visible_to_the_interceptor() -> Result<()> {
    let run = run_in_child("exec -l /usr/bin/touch @MARKER@")?;

    assert!(!run.marker().exists());
    assert_eq!(run.record.len(), 1, "record: {:?}", run.record);
    assert_eq!(
        run.record[0][2], "-/usr/bin/touch",
        "a login exec should present argv0 with a leading dash"
    );
    Ok(())
}

/// Sanity: the fixture really can create the marker when nothing denies it, so the
/// assertions above are not passing for the wrong reason.
#[test]
fn fixture_can_create_the_marker_when_unconfined() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("marker");
    let status = std::process::Command::new("/usr/bin/touch")
        .arg(&marker)
        .status()?;
    assert!(status.success());
    assert!(
        marker.exists(),
        "if this fails, the deny-side assertions prove nothing"
    );
    Ok(())
}
