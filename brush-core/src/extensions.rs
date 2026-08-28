//! Definition of shell behavior traits and defaults.

use crate::{Shell, error, extensions};

/// Trait for static shell extensions. Collects all associated types needed to
/// instantiate a shell into a single containing struct.
pub trait ShellExtensions: Clone + Default + Send + Sync + 'static {
    /// Type of the error behavior implementation.
    type ErrorFormatter: ErrorFormatter;

    /// Type of the command-interceptor implementation. See [`CommandInterceptor`].
    type CommandInterceptor: CommandInterceptor;
}

/// Shell extensions implementation constructed from component types.
#[derive(Clone, Default)]
pub struct ShellExtensionsImpl<
    EF: ErrorFormatter = DefaultErrorFormatter,
    CI: CommandInterceptor = DefaultCommandInterceptor,
> {
    _marker: std::marker::PhantomData<(EF, CI)>,
}

impl<EF: ErrorFormatter, CI: CommandInterceptor> ShellExtensions for ShellExtensionsImpl<EF, CI> {
    type ErrorFormatter = EF;
    type CommandInterceptor = CI;
}

/// Default shell extensions implementation.
/// This is a type alias for the most common shell configuration.
pub type DefaultShellExtensions =
    ShellExtensionsImpl<DefaultErrorFormatter, DefaultCommandInterceptor>;

/// Trait for defining shell error behaviors.
pub trait ErrorFormatter: Clone + Default + Send + Sync + 'static {
    /// Format the given error for display within the context of the provided shell.
    ///
    /// # Arguments
    ///
    /// * `error` - The error to format
    /// * `shell` - The shell context in which the error occurred.
    fn format_error(
        &self,
        error: &error::Error,
        shell: &Shell<impl extensions::ShellExtensions>,
    ) -> String {
        let _ = shell;
        std::format!("error: {error:#}\n")
    }
}

/// Default shell error behavior implementation.
#[derive(Clone, Default)]
pub struct DefaultErrorFormatter;

impl ErrorFormatter for DefaultErrorFormatter {}

/// Decision returned by [`CommandInterceptor::before_exec`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecDecision {
    /// Spawn the command.
    Allow,
    /// Do not spawn the command; fail it with the contained reason.
    Deny(String),
}

/// Hook for an embedding host to deny shell operations before they happen.
///
/// The default implementation ([`DefaultCommandInterceptor`]) allows everything, so a
/// shell that does not opt in behaves exactly as it does today.
///
/// Implementations must be cheap to clone: a pipeline stage runs against a clone of the
/// shell, which clones the interceptor with it. An interceptor with state should hold it
/// behind an [`Arc`](std::sync::Arc) so every clone observes the same state.
pub trait CommandInterceptor: Clone + Default + Send + Sync + 'static {
    /// Called immediately before an external command is spawned.
    ///
    /// A command name containing a path separator (`/bin/rm`, `./x`) bypasses both the
    /// `PATH` search and the builtin table, so gating on either is defeatable. This hook
    /// fires at the single external-spawn site that both dispatch branches funnel
    /// through, and cannot be circumvented by spelling a command differently.
    ///
    /// # Arguments
    ///
    /// * `program` - What is about to be executed: the resolved absolute path for a
    ///   `PATH` lookup, or the path as written for a path-separator command.
    /// * `args` - Arguments to the program, excluding `argv[0]`.
    fn before_exec(&self, program: &str, args: &[String]) -> ExecDecision {
        let _ = (program, args);
        ExecDecision::Allow
    }
}

/// Allow-all [`CommandInterceptor`]; equivalent to no interception.
#[derive(Clone, Default)]
pub struct DefaultCommandInterceptor;

impl CommandInterceptor for DefaultCommandInterceptor {}

/// Trait for placeholder behavior (stub for future extension).
pub trait PlaceholderBehavior: Clone + Default + Send + Sync + 'static {}

/// Default placeholder implementation.
#[derive(Clone, Default)]
pub struct DefaultPlaceholder;

impl PlaceholderBehavior for DefaultPlaceholder {}
