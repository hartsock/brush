use clap::Parser;
use std::{borrow::Cow, os::unix::process::CommandExt};

use brush_core::{
    ErrorKind, ExecutionExitCode, ExecutionResult, builtins, commands,
    filter::{CmdExecFilter as _, PreFilterResult, SimpleCmdParams},
    results::ExecutionWaitResult,
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

        // `exec` replaces the process image and never reaches external dispatch, so consult the
        // command filter here, showing it the resolved program that will actually run.
        let resolved = commands::resolve_external_program(context.shell, &self.args[0]);
        let program = resolved
            .as_deref()
            .map_or(Cow::Borrowed(self.args[0].as_str()), |p| {
                p.to_string_lossy()
            });
        let filter = context.shell.cmd_exec_filter().clone();
        let params = SimpleCmdParams::new(context.shell, program, &self.args[1..]);
        match filter.pre_simple_cmd(params).await {
            PreFilterResult::Continue(_) => {}
            PreFilterResult::Return(output) => {
                return match output?.wait().await? {
                    ExecutionWaitResult::Completed(result) => Ok(result),
                    ExecutionWaitResult::Stopped(_) => {
                        brush_core::error::unimp("filter returned a stopped process to exec")
                    }
                };
            }
            // Fail closed: about to replace the process image.
            _ => return brush_core::error::unimp("unrecognized filter result for exec"),
        }

        let mut cmd = commands::compose_std_command(
            &context,
            &self.args[0],
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
