//! Definition of shell behavior traits and defaults.

use std::ffi::OsStr;
use std::path::Path;

use crate::{Shell, error, extensions};

/// Trait for static shell extensions. Collects all associated types needed to
/// instantiate a shell into a single containing struct.
pub trait ShellExtensions: Clone + Default + Send + Sync + 'static {
    /// Type of the error behavior implementation.
    type ErrorFormatter: ErrorFormatter;
}

/// Shell extensions implementation constructed from component types.
#[derive(Clone, Default)]
pub struct ShellExtensionsImpl<EF: ErrorFormatter = DefaultErrorFormatter> {
    _marker: std::marker::PhantomData<EF>,
}

impl<EF: ErrorFormatter> ShellExtensions for ShellExtensionsImpl<EF> {
    type ErrorFormatter = EF;
}

/// Default shell extensions implementation.
/// This is a type alias for the most common shell configuration.
pub type DefaultShellExtensions = ShellExtensionsImpl<DefaultErrorFormatter>;

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

/// Identifies an external execution to [`CommandInterceptor::before_exec`].
///
/// The four fields are deliberately distinct concepts; a policy or audit log that conflates
/// them will be wrong for some spelling of some command:
///
/// * `command_name` — what was written in the shell program (`rm`, `/bin/rm`, `./x`).
/// * `program` — the executable that will actually run. For a bare name this is the
///   `PATH` lookup's result; for a name containing a path separator it is that name
///   resolved against the shell's working directory rules, *not* canonicalized.
/// * `argv0` — the zeroth argument the new process will see, which the caller can override
///   (`exec -a NAME cmd`) and which a login shell prefixes with `-`.
/// * `args` — the remaining arguments, excluding `argv0`.
///
/// `#[non_exhaustive]` so fields can be added without breaking implementors.
#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub struct ExecRequest<'a> {
    /// The command as written in the shell program.
    pub command_name: &'a str,
    /// The executable that will actually be run.
    pub program: &'a Path,
    /// The zeroth argument the new process will see.
    pub argv0: &'a OsStr,
    /// The arguments after `argv0`.
    pub args: &'a [&'a str],
}

impl<'a> ExecRequest<'a> {
    /// Creates a request. See the field docs for what each argument means.
    #[must_use]
    pub const fn new(
        command_name: &'a str,
        program: &'a Path,
        argv0: &'a OsStr,
        args: &'a [&'a str],
    ) -> Self {
        Self {
            command_name,
            program,
            argv0,
            args,
        }
    }
}

/// Decision returned by [`CommandInterceptor::before_exec`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecDecision {
    /// Run the command.
    Allow,
    /// Do not run the command; fail it with the contained reason.
    Deny(String),
}

/// Hook for an embedding host to authorize execution before it happens.
///
/// Install one with [`CreateOptions::command_interceptor`], or on an existing shell with
/// [`Shell::set_command_interceptor`]. A shell with no interceptor installed is
/// unconfined — absence is the default, so this trait has no allow-all implementation and
/// [`before_exec`](Self::before_exec) has no default body.
///
/// # What it covers
///
/// Every path on which brush itself launches or replaces a process image consults this
/// hook exactly once beforehand:
///
/// * ordinary external commands, whether resolved through `PATH` or written with a path
///   separator (`/bin/rm`, `./x`) — the latter reaches neither `PATH` nor the builtin
///   table, so a name-based gate outside the shell would not see it;
/// * the `exec` builtin outside a subshell, which replaces the shell process itself.
///
/// It does **not** cover what an already-running external process goes on to do; once a
/// program is admitted, its own `execve` calls and children are outside the shell.
///
/// # Sharing
///
/// The shell is cloned per pipeline stage, which clones the `Arc`, not the policy. Every
/// clone therefore consults one policy identity, and a policy that records decisions sees
/// all of them.
pub trait CommandInterceptor: Send + Sync {
    /// Called immediately before brush launches or replaces a process image.
    ///
    /// Returning [`ExecDecision::Deny`] prevents it. Implementors must not assume this is
    /// the only enforcement point in the host system; it is the only one *inside brush*.
    fn before_exec(&self, request: &ExecRequest<'_>) -> ExecDecision;
}

/// How a [`Shell`] is confined, if at all.
///
/// Three states, not two, because serialization cannot carry a policy: an `Arc<dyn Trait>`
/// has no serial form, and silently resuming a previously-confined shell as unconfined
/// would turn a serialization round-trip into a privilege escalation. What *is* serialized
/// is whether a policy was installed; a shell deserialized from a confined one comes back
/// [`AwaitingReinstall`](Self::AwaitingReinstall) and refuses to execute until the host
/// calls [`Shell::set_command_interceptor`] again.
#[derive(Clone, Default)]
pub enum InterceptorSlot {
    /// No policy was installed. Execution is unconfined; this is the default.
    #[default]
    Unconfined,
    /// A policy is installed and is consulted before every execution.
    Installed(std::sync::Arc<dyn CommandInterceptor>),
    /// This shell was deserialized from one that had a policy installed. The policy did not
    /// survive serialization, so execution is refused until one is reinstalled.
    AwaitingReinstall,
}

impl InterceptorSlot {
    /// Whether a policy was installed, or is awaited after deserialization. This is the one
    /// bit that survives serialization.
    #[must_use]
    pub const fn is_confined(&self) -> bool {
        matches!(self, Self::Installed(_) | Self::AwaitingReinstall)
    }
}

impl std::fmt::Debug for InterceptorSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconfined => f.write_str("Unconfined"),
            Self::Installed(_) => f.write_str("Installed(..)"),
            Self::AwaitingReinstall => f.write_str("AwaitingReinstall"),
        }
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for InterceptorSlot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Only the fact of confinement is representable; the policy itself is not.
        serializer.serialize_bool(self.is_confined())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for InterceptorSlot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A confined shell must not come back unconfined; it comes back refusing.
        Ok(if bool::deserialize(deserializer)? {
            Self::AwaitingReinstall
        } else {
            Self::Unconfined
        })
    }
}

/// Trait for placeholder behavior (stub for future extension).
pub trait PlaceholderBehavior: Clone + Default + Send + Sync + 'static {}

/// Default placeholder implementation.
#[derive(Clone, Default)]
pub struct DefaultPlaceholder;

impl PlaceholderBehavior for DefaultPlaceholder {}
