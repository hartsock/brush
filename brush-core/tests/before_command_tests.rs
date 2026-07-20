//! Integration tests for the per-command `CommandInterceptor::before_command`
//! hook on `ShellExtensions`.
//!
//! The hook exists to close a gap left by `before_exec`: a loop built entirely
//! out of builtins (`while true; do :; done`) never spawns anything, so it never
//! reaches the external-spawn site that `before_exec` guards. An embedding host
//! that needs to bound a run — a deadline, a cancellation flag, a step budget —
//! has no observation point inside such a loop and, critically, no way to stop
//! it. `before_command` fires once per command dispatch, builtins included, and
//! its `Deny` terminates the whole run rather than being folded into a
//! per-command exit status.
//!
//! These tests pin down all three of those properties: that the hook fires on
//! every iteration, that a `Deny` actually stops a non-terminating loop, and
//! that the default (no-op) interceptor leaves shell semantics untouched.

#![cfg(unix)]
#![cfg(test)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_used
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use brush_core::extensions::{
    CommandDecision, CommandInterceptor, DefaultErrorFormatter, ExecDecision, ShellExtensions,
};

/// How long a test is willing to wait for a run to stop. A correct
/// implementation finishes in milliseconds; this bound only exists so that a
/// regression shows up as a failure rather than as a hung test.
const WATCHDOG: Duration = Duration::from_secs(30);

/// An interceptor that allows the first `budget` command dispatches and denies
/// every one after that.
///
/// This models what an embedding host actually does: the per-command decision
/// is a single relaxed atomic operation, which is the cost budget the hot path
/// has to be able to afford.
#[derive(Clone, Default)]
struct BudgetInterceptor {
    seen: Arc<AtomicUsize>,
    budget: usize,
    /// When set, only this command name is ever denied; every other command is
    /// allowed unconditionally.
    ///
    /// This lets a test deny a loop's *body* while its *condition* keeps
    /// succeeding, which is the actual runaway shape. Denying the condition
    /// instead would end the loop for the uninteresting reason that a failed
    /// `while` condition ends a loop anyway.
    deny_only: Option<Arc<str>>,
}

impl BudgetInterceptor {
    fn new(budget: usize) -> Self {
        Self {
            seen: Arc::new(AtomicUsize::new(0)),
            budget,
            deny_only: None,
        }
    }

    fn deny_only(mut self, name: &str) -> Self {
        self.deny_only = Some(Arc::from(name));
        self
    }

    fn seen(&self) -> usize {
        self.seen.load(Ordering::Relaxed)
    }
}

impl CommandInterceptor for BudgetInterceptor {
    fn before_command(&self, name: &str) -> CommandDecision {
        let seen = self.seen.fetch_add(1, Ordering::Relaxed);

        if let Some(target) = self.deny_only.as_deref() {
            if name != target {
                return CommandDecision::Allow;
            }
        }

        if seen < self.budget {
            CommandDecision::Allow
        } else {
            CommandDecision::Deny(std::format!("command budget of {} exhausted", self.budget))
        }
    }
}

#[derive(Clone, Default)]
struct BudgetExtensions;
impl ShellExtensions for BudgetExtensions {
    type ErrorFormatter = DefaultErrorFormatter;
    type CommandInterceptor = BudgetInterceptor;
}

/// An interceptor that denies at the *external spawn* site only, leaving
/// `before_command` at its default. Used to pin the pre-existing (and
/// deliberately unchanged) behavior of `before_exec`.
#[derive(Clone, Default)]
struct ExecOnlyInterceptor {
    seen: Arc<AtomicUsize>,
}

impl CommandInterceptor for ExecOnlyInterceptor {
    fn before_exec(&self, _program: &str, _args: &[String]) -> ExecDecision {
        self.seen.fetch_add(1, Ordering::Relaxed);
        ExecDecision::Deny("denied by policy".to_string())
    }
}

#[derive(Clone, Default)]
struct ExecOnlyExtensions;
impl ShellExtensions for ExecOnlyExtensions {
    type ErrorFormatter = DefaultErrorFormatter;
    type CommandInterceptor = ExecOnlyInterceptor;
}

/// Builds a hermetic shell (no profile/rc, no inherited environment) with the
/// default builtin table registered, using the supplied interceptor.
async fn shell_with<SE: ShellExtensions>(
    interceptor: SE::CommandInterceptor,
) -> Result<brush_core::Shell<SE>> {
    let builtins = brush_builtins::default_builtins::<SE>(brush_builtins::BuiltinSet::BashMode);

    let shell = brush_core::Shell::builder_with_extensions::<SE>()
        .command_interceptor(interceptor)
        .builtins(builtins)
        .do_not_inherit_env(true)
        .skip_well_known_vars(true)
        .build()
        .await?;

    Ok(shell)
}

/// Runs `command` to completion, returning the interpreter's result verbatim so
/// callers can distinguish "finished with an exit status" from "the run was
/// terminated".
async fn run<SE: ShellExtensions>(
    shell: &mut brush_core::Shell<SE>,
    command: &str,
) -> Result<brush_core::ExecutionResult, brush_core::Error> {
    let params = shell.default_exec_params();
    shell
        .run_string(command, &brush_core::SourceInfo::default(), &params)
        .await
}

/// How a watchdogged run ended.
///
/// The outcome is reduced to plain data inside the worker thread so nothing
/// borrowed from the shell has to cross the channel.
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "payloads are carried solely so assertion failures are diagnosable via Debug"
)]
enum Outcome {
    /// The run completed and produced an exit status.
    Finished(u8),
    /// The run was terminated by a `before_command` denial.
    Denied,
    /// The run failed some other way.
    OtherError(String),
}

/// Runs `command` on a dedicated OS thread and waits at most [`WATCHDOG`] for
/// it to finish.
///
/// The watchdog lives on a *thread*, not on a tokio timer, on purpose: the
/// runaway these tests provoke is a CPU-bound shell loop that never yields, so
/// an in-runtime timer would simply be starved and never fire. Blocking on a
/// channel from outside the runtime is immune to that.
fn run_with_watchdog(interceptor: BudgetInterceptor, command: &'static str) -> Outcome {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("failed to build test runtime");

        let outcome = runtime.block_on(async move {
            let mut shell = match shell_with::<BudgetExtensions>(interceptor).await {
                Ok(shell) => shell,
                Err(e) => return Outcome::OtherError(e.to_string()),
            };

            match run(&mut shell, command).await {
                Ok(result) => Outcome::Finished(u8::from(result.exit_code)),
                Err(e) if matches!(e.kind(), brush_core::ErrorKind::CommandDenied(..)) => {
                    Outcome::Denied
                }
                Err(e) => Outcome::OtherError(e.to_string()),
            }
        });

        let _ = tx.send(outcome);
    });

    rx.recv_timeout(WATCHDOG).unwrap_or_else(|_| {
        panic!(
            "run did not terminate within {WATCHDOG:?}: a `before_command` denial \
             failed to stop the loop"
        )
    })
}

/// The load-bearing case: a loop made purely of builtins never terminates on
/// its own and never spawns anything, so `before_command` is the host's only
/// point of control. A `Deny` from it must stop the run.
///
/// Only the loop *body* (`:`) is denied; the `while true` condition keeps
/// succeeding throughout. That distinction is what makes this a real runaway —
/// if the denial were merely converted into a per-command exit status (as a
/// `before_exec` denial is), the loop would spin forever and the watchdog would
/// fire.
#[test]
fn deny_stops_a_pure_builtin_infinite_loop() {
    let interceptor = BudgetInterceptor::new(100).deny_only(":");
    let probe = interceptor.clone();

    let outcome = run_with_watchdog(interceptor, "while true; do :; done");

    assert!(
        matches!(outcome, Outcome::Denied),
        "a `before_command` denial must propagate out of the interpreter as an \
         error; if it is folded into an exit status the enclosing `while` keeps \
         spinning. Got: {outcome:?}"
    );

    // Confirm the loop really did iterate before being stopped, so that we
    // measured termination rather than a shell that never got started.
    assert!(
        probe.seen() > 100,
        "expected the loop to run through its budget; saw {} dispatches",
        probe.seen()
    );
}

/// The same termination guarantee must hold when the runaway is inside a
/// subshell, which otherwise contains errors rather than letting them escape.
#[test]
fn deny_stops_a_runaway_inside_a_subshell() {
    let interceptor = BudgetInterceptor::new(100).deny_only(":");

    let outcome = run_with_watchdog(interceptor, "(while true; do :; done)");

    assert!(
        matches!(outcome, Outcome::Denied),
        "a terminating denial must escape the subshell boundary. Got: {outcome:?}"
    );
}

/// The hook must fire for builtins, once per loop iteration — not just for
/// external commands, and not just once for the loop as a whole.
#[tokio::test]
async fn hook_fires_once_per_builtin_dispatch_including_each_iteration() -> Result<()> {
    let interceptor = BudgetInterceptor::new(usize::MAX);
    let probe = interceptor.clone();
    let mut shell = shell_with::<BudgetExtensions>(interceptor).await?;

    // Ten iterations, each dispatching exactly one builtin (`:`) in the body,
    // plus the `[` condition evaluated eleven times and two assignments'
    // worth of surrounding commands. We assert a lower bound rather than an
    // exact count so the test does not encode incidental dispatch details.
    let result = run(
        &mut shell,
        "x=0; while [ $x -lt 10 ]; do x=$((x+1)); :; done",
    )
    .await?;

    assert_eq!(
        u8::from(result.exit_code),
        0,
        "the loop should have completed normally"
    );
    assert!(
        probe.seen() >= 21,
        "expected at least 21 dispatches (11 conditions + 10 bodies); saw {}",
        probe.seen()
    );

    Ok(())
}

/// An interceptor that always allows must be semantically invisible: loops,
/// exit statuses, and control flow are unchanged.
#[tokio::test]
async fn allow_decisions_leave_shell_semantics_unchanged() -> Result<()> {
    let interceptor = BudgetInterceptor::new(usize::MAX);
    let mut shell = shell_with::<BudgetExtensions>(interceptor).await?;

    // A counting loop that self-checks its own result, so a wrong answer shows
    // up as a non-zero exit status without needing to capture output.
    let result = run(
        &mut shell,
        "x=0; while [ $x -lt 5 ]; do x=$((x+1)); done; [ $x -eq 5 ]",
    )
    .await?;
    assert_eq!(u8::from(result.exit_code), 0, "counting loop misbehaved");

    // `break`, `continue`, and non-zero exit statuses still work.
    let result = run(
        &mut shell,
        "n=0; for i in 1 2 3 4 5; do if [ $i -eq 2 ]; then continue; fi; \
         if [ $i -eq 4 ]; then break; fi; n=$((n+1)); done; [ $n -eq 2 ]",
    )
    .await?;
    assert_eq!(u8::from(result.exit_code), 0, "break/continue misbehaved");

    let result = run(&mut shell, "false").await?;
    assert_eq!(u8::from(result.exit_code), 1, "exit status not preserved");

    Ok(())
}

/// The *default* interceptor is a no-op, so a shell that installs none behaves
/// exactly as before this hook existed.
#[tokio::test]
async fn default_interceptor_is_a_no_op() -> Result<()> {
    let builtins = brush_builtins::default_builtins::<brush_core::extensions::DefaultShellExtensions>(
        brush_builtins::BuiltinSet::BashMode,
    );
    let mut shell = brush_core::Shell::builder()
        .builtins(builtins)
        .do_not_inherit_env(true)
        .skip_well_known_vars(true)
        .build()
        .await?;

    let params = shell.default_exec_params();
    let result = shell
        .run_string(
            "x=0; while [ $x -lt 5 ]; do x=$((x+1)); done; [ $x -eq 5 ]",
            &brush_core::SourceInfo::default(),
            &params,
        )
        .await?;

    assert_eq!(u8::from(result.exit_code), 0);
    Ok(())
}

/// Regression pin for the *unchanged* behavior of `before_exec`: its denial is
/// still a per-command failure (exit 126) that the enclosing loop recovers
/// from and continues past.
///
/// This is precisely the limitation that motivated `before_command`. Changing
/// `before_exec` to terminate instead would break existing consumers that rely
/// on denying one command without tearing down the run, so the two hooks
/// deliberately differ.
#[tokio::test]
async fn before_exec_denial_remains_a_recoverable_per_command_failure() -> Result<()> {
    let interceptor = ExecOnlyInterceptor::default();
    let probe = interceptor.clone();
    let mut shell = shell_with::<ExecOnlyExtensions>(interceptor).await?;

    let result = run(&mut shell, "for i in 1 2 3 4 5; do /bin/rm; done").await?;

    assert_eq!(
        u8::from(result.exit_code),
        126,
        "a denied exec should still report `cannot execute`"
    );
    assert_eq!(
        probe.seen.load(Ordering::Relaxed),
        5,
        "every iteration should still have been attempted"
    );

    Ok(())
}
