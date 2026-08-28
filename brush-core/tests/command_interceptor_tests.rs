//! Integration tests for [`CommandInterceptor::before_exec`].
//!
//! Denied programs use `/bin/rm`; whether it exists is irrelevant, because the hook denies
//! it before any spawn. Programs that are meant to run use `/usr/bin/true` and
//! `/usr/bin/touch`, which exist on both Linux and macOS (`/bin/true` does not exist on
//! macOS).
#![cfg(unix)]
#![cfg(test)]
#![allow(clippy::panic_in_result_fn)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use brush_core::extensions::{CommandInterceptor, ErrorFormatter, ExecDecision, ShellExtensions};

/// Every `before_exec` consultation: the program, and its args without `argv[0]`.
type ExecCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

/// Denies any program whose basename is in the deny list, and records every consultation
/// so tests can assert the hook actually fired and saw what the docs promise.
#[derive(Clone, Default)]
struct PolicyInterceptor {
    denied: Arc<Vec<String>>,
    calls: ExecCalls,
}

impl PolicyInterceptor {
    fn denying(basenames: &[&str]) -> Self {
        Self {
            denied: Arc::new(basenames.iter().map(|s| (*s).to_string()).collect()),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }

    fn programs(&self) -> Vec<String> {
        self.calls().into_iter().map(|(p, _)| p).collect()
    }
}

impl CommandInterceptor for PolicyInterceptor {
    fn before_exec(&self, program: &str, args: &[String]) -> ExecDecision {
        self.calls
            .lock()
            .unwrap()
            .push((program.to_string(), args.to_vec()));

        // Match on the basename so `rm`, `/bin/rm` and `./rm` are all caught.
        let basename = Path::new(program)
            .file_name()
            .map_or_else(|| program.to_string(), |s| s.to_string_lossy().into_owned());

        if self.denied.iter().any(|d| d == &basename) {
            ExecDecision::Deny(format!("'{basename}' is not permitted by policy"))
        } else {
            ExecDecision::Allow
        }
    }
}

#[derive(Clone, Default)]
struct PolicyExtensions;

impl ShellExtensions for PolicyExtensions {
    type ErrorFormatter = DefaultFormatter;
    type CommandInterceptor = PolicyInterceptor;
}

#[derive(Clone, Default)]
struct DefaultFormatter;
impl ErrorFormatter for DefaultFormatter {}

/// A hermetic shell wired to `interceptor`. No builtins are registered: every command
/// these tests run is an external, which is exactly the surface `before_exec` guards.
async fn shell_with(interceptor: PolicyInterceptor) -> Result<brush_core::Shell<PolicyExtensions>> {
    let mut shell = brush_core::Shell::builder_with_extensions::<PolicyExtensions>()
        .command_interceptor(interceptor)
        .do_not_inherit_env(true)
        .skip_well_known_vars(true)
        .build()
        .await?;

    // Deterministic PATH, set directly rather than via `export`, so the test needs no
    // builtin table.
    shell.set_env_global(
        "PATH",
        brush_core::variables::ShellVariable::new("/bin:/usr/bin"),
    )?;

    Ok(shell)
}

async fn run(shell: &mut brush_core::Shell<PolicyExtensions>, cmd: &str) -> Result<u8> {
    let params = shell.default_exec_params();
    let result = shell
        .run_string(cmd, &brush_core::SourceInfo::default(), &params)
        .await?;
    Ok(u8::from(result.exit_code))
}

/// A bare name, resolved through `PATH`.
#[tokio::test]
async fn denies_bare_name_command() -> Result<()> {
    let interceptor = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(interceptor.clone()).await?;

    assert_ne!(run(&mut shell, "rm /tmp/does-not-matter").await?, 0);
    assert!(
        interceptor.programs().iter().any(|p| p.ends_with("rm")),
        "before_exec should have been consulted; saw: {:?}",
        interceptor.programs()
    );
    Ok(())
}

/// The load-bearing case: an absolute path bypasses both `PATH` and the builtin table.
#[tokio::test]
async fn denies_absolute_path_command() -> Result<()> {
    let interceptor = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(interceptor.clone()).await?;

    assert_ne!(run(&mut shell, "/bin/rm /tmp/does-not-matter").await?, 0);
    assert!(
        interceptor.programs().iter().any(|p| p == "/bin/rm"),
        "before_exec should have seen the path as written; saw: {:?}",
        interceptor.programs()
    );
    Ok(())
}

/// The other path-separator spelling the hook's docs promise to cover.
#[tokio::test]
async fn denies_relative_path_command() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let interceptor = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(interceptor.clone()).await?;
    shell.set_working_dir(dir.path())?;

    assert_ne!(run(&mut shell, "./rm /tmp/does-not-matter").await?, 0);
    assert!(
        interceptor.programs().iter().any(|p| p == "./rm"),
        "before_exec should have seen the relative path; saw: {:?}",
        interceptor.programs()
    );
    Ok(())
}

/// A denial must prevent the spawn, not merely report an error afterwards. `touch` is
/// chosen because its only effect is observable from the test.
#[tokio::test]
async fn denied_command_never_runs() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let marker: PathBuf = dir.path().join("marker");

    let interceptor = PolicyInterceptor::denying(&["touch"]);
    let mut shell = shell_with(interceptor).await?;

    assert_ne!(
        run(&mut shell, &format!("/usr/bin/touch {}", marker.display())).await?,
        0
    );
    assert!(
        !marker.exists(),
        "the denied program must not have executed, but it created {}",
        marker.display()
    );
    Ok(())
}

/// A denial is reported as "cannot execute".
#[tokio::test]
async fn denial_exits_with_126() -> Result<()> {
    let interceptor = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(interceptor).await?;

    assert_eq!(run(&mut shell, "/bin/rm /tmp/does-not-matter").await?, 126);
    Ok(())
}

/// The hook's documented contract: a `PATH` lookup passes the *resolved* absolute path,
/// and `args` excludes `argv[0]`.
#[tokio::test]
async fn hook_sees_resolved_path_and_args_without_argv0() -> Result<()> {
    let interceptor = PolicyInterceptor::denying(&[]);
    let mut shell = shell_with(interceptor.clone()).await?;

    assert_eq!(run(&mut shell, "true alpha beta").await?, 0);

    let calls = interceptor.calls();
    assert!(
        calls
            .iter()
            .any(|(p, _)| p.ends_with("true") && Path::new(p).is_absolute()),
        "a PATH-resolved command should reach the hook as an absolute path; saw: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|(p, args)| p.ends_with("true") && args.as_slice() == ["alpha", "beta"]),
        "args should be the arguments without argv[0]; saw: {calls:?}"
    );
    Ok(())
}

/// Command substitution reaches the same spawn funnel.
#[tokio::test]
async fn denial_applies_in_command_substitution() -> Result<()> {
    let interceptor = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(interceptor.clone()).await?;

    let _ = run(&mut shell, "x=$(/bin/rm /tmp/does-not-matter)").await?;
    assert!(
        interceptor.programs().iter().any(|p| p == "/bin/rm"),
        "command substitution must not bypass before_exec; saw: {:?}",
        interceptor.programs()
    );
    Ok(())
}

/// A subshell reaches the same spawn funnel.
#[tokio::test]
async fn denial_applies_in_subshell() -> Result<()> {
    let interceptor = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(interceptor.clone()).await?;

    let _ = run(&mut shell, "( /bin/rm /tmp/does-not-matter )").await?;
    assert!(
        interceptor.programs().iter().any(|p| p == "/bin/rm"),
        "a subshell must not bypass before_exec; saw: {:?}",
        interceptor.programs()
    );
    Ok(())
}

/// A non-final pipeline stage runs against a *clone* of the shell, which clones the
/// interceptor with it. This pins the documented requirement that a stateful interceptor
/// stays coherent across those clones.
#[tokio::test]
async fn denial_applies_in_a_cloned_pipeline_stage() -> Result<()> {
    let interceptor = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(interceptor.clone()).await?;

    let _ = run(&mut shell, "/bin/rm /tmp/does-not-matter | /usr/bin/true").await?;
    assert!(
        interceptor.programs().iter().any(|p| p == "/bin/rm"),
        "a pipeline stage running on a cloned shell must still consult the interceptor, \
         and the clone must share the original's state; saw: {:?}",
        interceptor.programs()
    );
    Ok(())
}

/// A permitted command runs normally.
#[tokio::test]
async fn allows_permitted_command() -> Result<()> {
    let interceptor = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(interceptor.clone()).await?;

    assert_eq!(run(&mut shell, "/usr/bin/true").await?, 0);
    assert!(
        interceptor.programs().iter().any(|p| p == "/usr/bin/true"),
        "before_exec should have been consulted; saw: {:?}",
        interceptor.programs()
    );
    Ok(())
}
