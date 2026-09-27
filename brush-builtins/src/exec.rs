use clap::Parser;
use std::{
    borrow::Cow,
    ffi::OsStr,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
};

use brush_core::{
    ErrorKind, ExecutionExitCode, ExecutionResult, builtins, commands, extensions::ExecRequest,
};

/// Exec the provided command.
#[derive(Parser)]
pub(crate) struct ExecCommand {
    /// Pass given name as zeroth argument to command.
    #[arg(short = 'a', value_name = "NAME")]
    name_for_argv0: Option<String>,

    /// Exec command with an empty environment.
    #[arg(short = 'c')]
    empty_environment: bool,

    /// Exec command as a login shell.
    #[arg(short = 'l')]
    exec_as_login: bool,

    /// Command and args.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for ExecCommand {
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<ExecutionResult, Self::Error> {
        if self.args.is_empty() {
            // When no arguments are present, then there's nothing for us to execute -- but we need
            // to ensure that any redirections setup for this builtin get applied to the calling
            // shell instance.
            #[allow(clippy::needless_collect)]
            let fds: Vec<_> = context.iter_fds().collect();

            context.shell.replace_open_files(fds.into_iter());
            return Ok(ExecutionResult::success());
        }

        // If we know we're already running in a subshell, then `exec`ing is actually
        // unsafe, since it would also replace the *parent* shell instance. We instead
        // delegate to the `command` builtin to perform the execution, with an expectation
        // of returning.
        if context.shell.is_subshell() {
            if self.empty_environment || self.exec_as_login || self.name_for_argv0.is_some() {
                return brush_core::error::unimp("exec with options in subshell not yet supported");
            }

            let cmd_cmd = crate::command::CommandCommand {
                command_and_args: self.args.clone(),
                ..Default::default()
            };

            return cmd_cmd.execute(context).await;
        }

        let mut argv0 = Cow::Borrowed(self.name_for_argv0.as_ref().unwrap_or(&self.args[0]));

        if self.exec_as_login {
            argv0 = Cow::Owned(std::format!("-{argv0}"));
        }

        // `exec` replaces this process image outright, so it never reaches
        // `commands::execute_external_command`. Authorize here, before `cmd.exec()`, using
        // the same brush-core helper that path uses -- this builtin must not interpret
        // policy itself.
        //
        // When confined we resolve the program first and launch *that* exact path, so what
        // the interceptor authorized is what runs; the OS would otherwise redo its own
        // `PATH` search against the child environment, which need not match the shell's.
        // Unconfined, behavior is byte-for-byte what it was before.
        let mut program_to_launch: Cow<'_, str> = Cow::Borrowed(self.args[0].as_str());
        if context.shell.command_interceptor().is_confined() {
            let resolved: Option<PathBuf> =
                commands::resolve_external_program(context.shell, &self.args[0]);
            let program: &Path = resolved
                .as_deref()
                .unwrap_or_else(|| Path::new(self.args[0].as_str()));

            let hook_args: Vec<&str> = self.args[1..].iter().map(String::as_str).collect();
            commands::authorize_execution(
                context.shell,
                &ExecRequest::new(
                    self.args[0].as_str(),
                    program,
                    OsStr::new(argv0.as_ref().as_str()),
                    hook_args.as_slice(),
                ),
            )?;

            if let Some(resolved) = resolved {
                program_to_launch = Cow::Owned(resolved.to_string_lossy().into_owned());
            }
        }

        let mut cmd = commands::compose_std_command(
            &context,
            program_to_launch.as_ref(),
            argv0.as_str(),
            &self.args[1..],
            self.empty_environment,
        )?;

        let exec_error = cmd.exec();

        if exec_error.kind() == std::io::ErrorKind::NotFound {
            Ok(ExecutionExitCode::NotFound.into())
        } else {
            Err(ErrorKind::from(exec_error).into())
        }
    }
}
