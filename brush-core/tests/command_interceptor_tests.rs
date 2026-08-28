//! Integration tests for [`CommandInterceptor::before_exec`].
//!
//! Denied programs use `/bin/rm`; whether it exists is irrelevant, because the hook denies
//! it before any spawn. Programs meant to run use `/usr/bin/true` and `/usr/bin/touch`,
//! which exist on both Linux and macOS (`/bin/true` does not exist on macOS).
#![cfg(unix)]
#![cfg(test)]
#![allow(clippy::panic_in_result_fn)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use brush_core::extensions::{CommandInterceptor, ExecDecision, ExecRequest, InterceptorSlot};

/// One recorded consultation, as the four distinct concepts the request carries:
/// `(command_name, program, argv0, args)`.
type Consultation = (String, String, String, Vec<String>);

/// Denies any program whose basename is in the deny list, and records every consultation
/// so tests can assert the hook fired and saw what the docs promise.
#[derive(Default)]
struct PolicyInterceptor {
    denied: Vec<String>,
    /// Every consultation, in order.
    calls: Mutex<Vec<Consultation>>,
}

impl PolicyInterceptor {
    fn denying(basenames: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            denied: basenames.iter().map(|s| (*s).to_string()).collect(),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<Consultation> {
        self.calls.lock().unwrap().clone()
    }

    /// The resolved programs, which is what most cases assert on.
    fn programs(&self) -> Vec<String> {
        self.calls().into_iter().map(|(_, p, _, _)| p).collect()
    }
}

impl CommandInterceptor for PolicyInterceptor {
    fn before_exec(&self, request: &ExecRequest<'_>) -> ExecDecision {
        self.calls.lock().unwrap().push((
            request.command_name.to_string(),
            request.program.display().to_string(),
            request.argv0.to_string_lossy().into_owned(),
            request.args.iter().map(|a| (*a).to_string()).collect(),
        ));

        // Match on the basename so `rm`, `/bin/rm` and `./rm` are all caught.
        let basename = request.program.file_name().map_or_else(
            || request.program.display().to_string(),
            |s| s.to_string_lossy().into_owned(),
        );

        if self.denied.iter().any(|d| d == &basename) {
            ExecDecision::Deny(format!("'{basename}' is not permitted by policy"))
        } else {
            ExecDecision::Allow
        }
    }
}

/// A hermetic shell wired to `interceptor`, using brush's default extensions: installing a
/// policy requires no custom `ShellExtensions`. No builtins are registered, because every
/// command these tests run is an external -- exactly the surface `before_exec` guards.
async fn shell_with(interceptor: Arc<PolicyInterceptor>) -> Result<brush_core::Shell> {
    let mut shell = brush_core::Shell::builder()
        .command_interceptor(interceptor)
        .do_not_inherit_env(true)
        .skip_well_known_vars(true)
        .build()
        .await?;

    // Deterministic PATH, set directly rather than via `export`, so no builtin table is
    // needed.
    shell.set_env_global(
        "PATH",
        brush_core::variables::ShellVariable::new("/bin:/usr/bin"),
    )?;

    Ok(shell)
}

async fn run(shell: &mut brush_core::Shell, cmd: &str) -> Result<u8> {
    let params = shell.default_exec_params();
    let result = shell
        .run_string(cmd, &brush_core::SourceInfo::default(), &params)
        .await?;
    Ok(u8::from(result.exit_code))
}

/// A bare name, resolved through `PATH`.
#[tokio::test]
async fn denies_bare_name_command() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy.clone()).await?;

    assert_ne!(run(&mut shell, "rm /tmp/does-not-matter").await?, 0);
    assert!(
        policy.programs().iter().any(|p| p.ends_with("rm")),
        "before_exec should have been consulted; saw: {:?}",
        policy.programs()
    );
    Ok(())
}

/// The load-bearing case: an absolute path bypasses both `PATH` and the builtin table.
#[tokio::test]
async fn denies_absolute_path_command() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy.clone()).await?;

    assert_ne!(run(&mut shell, "/bin/rm /tmp/does-not-matter").await?, 0);
    assert!(
        policy.programs().iter().any(|p| p == "/bin/rm"),
        "before_exec should have seen the path as written; saw: {:?}",
        policy.programs()
    );
    Ok(())
}

/// The other path-separator spelling the hook's docs promise to cover.
#[tokio::test]
async fn denies_relative_path_command() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy.clone()).await?;
    shell.set_working_dir(dir.path())?;

    assert_ne!(run(&mut shell, "./rm /tmp/does-not-matter").await?, 0);
    assert!(
        policy.programs().iter().any(|p| p == "./rm"),
        "before_exec should have seen the relative path; saw: {:?}",
        policy.programs()
    );
    Ok(())
}

/// A denial must prevent the spawn, not merely report an error afterwards. `touch` is
/// chosen because its only effect is observable from the test.
#[tokio::test]
async fn denied_command_never_runs() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let marker: PathBuf = dir.path().join("marker");

    let policy = PolicyInterceptor::denying(&["touch"]);
    let mut shell = shell_with(policy).await?;

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
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy).await?;

    assert_eq!(run(&mut shell, "/bin/rm /tmp/does-not-matter").await?, 126);
    Ok(())
}

/// The documented contract: a `PATH` lookup passes the *resolved* absolute path, and
/// `args` excludes `argv[0]`.
#[tokio::test]
async fn hook_sees_resolved_path_and_args_without_argv0() -> Result<()> {
    let policy = PolicyInterceptor::denying(&[]);
    let mut shell = shell_with(policy.clone()).await?;

    assert_eq!(run(&mut shell, "true alpha beta").await?, 0);

    let calls = policy.calls();
    assert!(
        calls
            .iter()
            .any(|(_, program, _, _)| program.ends_with("true")
                && Path::new(program).is_absolute()),
        "a PATH-resolved command should reach the hook as an absolute path; saw: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|(_, program, _, args)| program.ends_with("true")
                && args.as_slice() == ["alpha", "beta"]),
        "args should be the arguments without argv[0]; saw: {calls:?}"
    );
    Ok(())
}

/// Command substitution reaches the same spawn funnel.
#[tokio::test]
async fn denial_applies_in_command_substitution() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy.clone()).await?;

    let _ = run(&mut shell, "x=$(/bin/rm /tmp/does-not-matter)").await?;
    assert!(
        policy.programs().iter().any(|p| p == "/bin/rm"),
        "command substitution must not bypass before_exec; saw: {:?}",
        policy.programs()
    );
    Ok(())
}

/// A subshell reaches the same spawn funnel.
#[tokio::test]
async fn denial_applies_in_subshell() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy.clone()).await?;

    let _ = run(&mut shell, "( /bin/rm /tmp/does-not-matter )").await?;
    assert!(
        policy.programs().iter().any(|p| p == "/bin/rm"),
        "a subshell must not bypass before_exec; saw: {:?}",
        policy.programs()
    );
    Ok(())
}

/// A non-final pipeline stage runs against a *clone* of the shell. The interceptor is held
/// behind an `Arc`, so every clone consults the same policy and records into the same log.
#[tokio::test]
async fn denial_applies_in_a_cloned_pipeline_stage() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy.clone()).await?;

    let _ = run(&mut shell, "/bin/rm /tmp/does-not-matter | /usr/bin/true").await?;
    assert!(
        policy.programs().iter().any(|p| p == "/bin/rm"),
        "a pipeline stage running on a cloned shell must still consult the interceptor; \
         saw: {:?}",
        policy.programs()
    );
    Ok(())
}

/// A permitted command runs normally.
#[tokio::test]
async fn allows_permitted_command() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy.clone()).await?;

    assert_eq!(run(&mut shell, "/usr/bin/true").await?, 0);
    assert!(
        policy.programs().iter().any(|p| p == "/usr/bin/true"),
        "before_exec should have been consulted; saw: {:?}",
        policy.programs()
    );
    Ok(())
}

/// `command_name` is the spelling entered; `program` is what will actually run. Conflating
/// them makes a policy wrong for one spelling or the other.
#[tokio::test]
async fn request_separates_command_name_from_resolved_program() -> Result<()> {
    let policy = PolicyInterceptor::denying(&[]);
    let mut shell = shell_with(policy.clone()).await?;

    assert_eq!(run(&mut shell, "true").await?, 0);

    let calls = policy.calls();
    assert!(
        calls.iter().any(|(command_name, program, _, _)| {
            command_name == "true" && Path::new(program).is_absolute() && program != command_name
        }),
        "a bare name should arrive with command_name as written and program resolved; \
         saw: {calls:?}"
    );
    Ok(())
}

/// Every clone of a shell must consult *one* policy identity, not a copy. A pipeline stage
/// runs against a clone, so a policy that counts or records decisions would otherwise see
/// only some of them.
#[tokio::test]
async fn shell_clones_share_one_policy_identity() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let shell = shell_with(policy.clone()).await?;
    let mut clone = shell.clone();

    let installed_ptr = |shell: &brush_core::Shell| match shell.command_interceptor() {
        InterceptorSlot::Installed(interceptor) => Some(Arc::as_ptr(interceptor).cast::<()>()),
        _ => None,
    };
    assert!(
        installed_ptr(&shell).is_some(),
        "the original shell should have a policy installed"
    );
    assert_eq!(
        installed_ptr(&shell),
        installed_ptr(&clone),
        "a shell clone must share the original's policy, not copy it"
    );

    // And the shared identity is observable: a decision made through the clone is visible
    // through the handle the test still holds.
    assert_ne!(run(&mut clone, "/bin/rm /tmp/does-not-matter").await?, 0);
    assert!(
        policy.programs().iter().any(|p| p == "/bin/rm"),
        "the clone's decision should be recorded in the shared policy; saw: {:?}",
        policy.programs()
    );
    Ok(())
}

/// A denial is reported as a `PermissionDenied` `io::Error` carrying an `ExecDeniedError`,
/// so a host can recover the interceptor's own reason by downcasting rather than by parsing
/// a rendered message.
///
/// This drives `authorize_execution` directly. Going through `run_string` would not test
/// it: the shell converts the error to exit status 126 and prints it, so the payload is not
/// observable there -- which is exactly why asserting on `run_string` here would pass
/// without proving anything.
#[tokio::test]
async fn denial_reason_is_recoverable_by_downcast() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let shell = shell_with(policy).await?;

    let program = Path::new("/bin/rm");
    let request = ExecRequest::new(
        "/bin/rm",
        program,
        std::ffi::OsStr::new("/bin/rm"),
        &["/tmp/does-not-matter"],
    );

    let Err(err) = brush_core::commands::authorize_execution(&shell, &request) else {
        return Err(anyhow::anyhow!("a denied request should not be authorized"));
    };

    // Exit status stays 126 ("cannot execute"), as it was before this error carried a payload.
    assert_eq!(
        u8::from(brush_core::ExecutionExitCode::from(&err)),
        126,
        "a denial must still report 126"
    );

    // Walk the source chain for the two documented properties.
    let mut hop: Option<&(dyn std::error::Error + 'static)> = Some(&err);
    let mut permission_denied = false;
    let mut reason = None;
    while let Some(current) = hop {
        if let Some(io_err) = current.downcast_ref::<std::io::Error>() {
            if io_err.kind() == std::io::ErrorKind::PermissionDenied {
                permission_denied = true;
            }
            // N.B. `io::Error::source()` skips over the custom payload and returns *its*
            // source, so the payload is only reachable through `get_ref()`.
            if let Some(denied) = io_err
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<brush_core::error::ExecDeniedError>())
            {
                reason = Some(denied.reason().to_string());
            }
        }
        hop = current.source();
    }

    assert!(
        permission_denied,
        "the denial should surface as io::ErrorKind::PermissionDenied; got: {err:?}"
    );
    assert_eq!(
        reason.as_deref(),
        Some("'rm' is not permitted by policy"),
        "the interceptor's own reason should be recoverable by downcast; got: {err:?}"
    );
    Ok(())
}

/// Serialization cannot carry an `Arc<dyn Trait>`. A confined shell must therefore not come
/// back unconfined: it comes back refusing, until the host reinstalls a policy.
#[cfg(feature = "serde")]
#[tokio::test]
async fn a_deserialized_confined_shell_refuses_until_the_policy_is_reinstalled() -> Result<()> {
    // The slot is what carries the one serializable bit.
    let confined = InterceptorSlot::Installed(PolicyInterceptor::denying(&[]));
    let json = serde_json::to_string(&confined)?;
    assert_eq!(
        json, "true",
        "a confined shell should serialize as confined"
    );

    let restored: InterceptorSlot = serde_json::from_str(&json)?;
    assert!(
        matches!(restored, InterceptorSlot::AwaitingReinstall),
        "a confined shell must not deserialize back to Unconfined; got {restored:?}"
    );
    assert!(
        restored.is_confined(),
        "an awaiting-reinstall shell still counts as confined"
    );

    // An unconfined shell round-trips as unconfined.
    let json = serde_json::to_string(&InterceptorSlot::Unconfined)?;
    assert_eq!(json, "false");
    let restored: InterceptorSlot = serde_json::from_str(&json)?;
    assert!(matches!(restored, InterceptorSlot::Unconfined));
    Ok(())
}

/// The reinjection contract: `set_command_interceptor` installs a policy on an existing
/// shell, and removing it is an explicit act rather than a silent default.
#[tokio::test]
async fn set_command_interceptor_installs_and_removes() -> Result<()> {
    let policy = PolicyInterceptor::denying(&["rm"]);
    let mut shell = shell_with(policy.clone()).await?;

    assert_ne!(run(&mut shell, "/bin/rm /tmp/does-not-matter").await?, 0);
    let denied_once = policy.programs().len();

    // Explicitly unconfine.
    shell.set_command_interceptor(None);
    assert!(
        !shell.command_interceptor().is_confined(),
        "removing the policy should leave the shell unconfined"
    );
    assert_eq!(run(&mut shell, "/usr/bin/true").await?, 0);
    assert_eq!(
        policy.programs().len(),
        denied_once,
        "an uninstalled policy must not be consulted"
    );

    // Reinstall, and confirm it is consulted again.
    shell.set_command_interceptor(Some(policy.clone()));
    assert!(shell.command_interceptor().is_confined());
    assert_ne!(run(&mut shell, "/bin/rm /tmp/does-not-matter").await?, 0);
    assert!(policy.programs().len() > denied_once);
    Ok(())
}
