//! Python bindings for the `brush` shell, wrapping the embeddable `brush_core::Shell` API.
//!
//! Design notes (grounded in the brush-core investigation, signatures VERIFIED against
//! brush-core 0.5.0 source):
//!  * brush-core's execution API is fully ASYNC/Tokio (`run_string` execution.rs:163,
//!    `run_script` execution.rs:213, `build()` builder.rs:17). We own a multi-thread
//!    `tokio::runtime::Runtime` and `block_on` each call, exactly like
//!    brush-core/examples/call-func.rs.
//!  * We wrap every blocking `block_on` in `Python::allow_threads` so the GIL is
//!    released while the shell blocks on child processes / I/O.
//!  * A bare `Shell::builder().build()` has NO builtins; we add
//!    `default_builtins(BuiltinSet::BashMode)` from the separate brush-builtins crate.
//!  * Output capture uses TEMPFILES (`std::fs::File` -> `OpenFile::File(Arc<File>)`,
//!    VERIFIED `impl From<std::fs::File> for OpenFile` openfiles.rs:214). Tempfiles avoid
//!    the ~64KB OS-pipe deadlock that a naive `PipeWriter` capture hits; brush-core's
//!    concurrent drainer (`AsyncPipeReader`) is `pub(crate)` and unreachable out-of-tree.
//!    `TryFrom<OpenFile> for Stdio` `try_clone`s the File so EXTERNAL commands are
//!    captured too (VERIFIED openfiles.rs:242).
//!  * Syntax/parse errors are reported bash-style (VERIFIED empirically against
//!    brush-core 0.5.0): `run_string` returns `Ok` with `exit_code == 2` and the
//!    parser message on stderr -- it does NOT raise. A Python exception is raised only
//!    when brush-core returns a Rust `Err` (lower-level execution/IO failures). So
//!    callers should check `.exit_code` / `.success`, not rely on exceptions for bad
//!    syntax.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::PathBuf;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

// Builtins extension trait: adds `.default_builtins(set)` to the ShellBuilder.
// (brush-builtins/src/builder.rs:11, factory.rs -- VERIFIED.)
use brush_builtins::{BuiltinSet, ShellBuilderExt as _};

// Core embedding surface (all VERIFIED against brush-core 0.5.0 source):
//   brush_core::Shell                          (lib.rs:59; = Shell<DefaultShellExtensions>)
//   Shell::builder()                           (builder.rs:272, sync)
//   ShellBuilder::build().await                (builder.rs:17, async)
//   Shell::run_string(cmd, &SourceInfo, &params).await   (execution.rs:163)
//   Shell::run_script(path, args_iter).await   (execution.rs:213)
//   Shell::default_exec_params() -> ExecutionParameters  (execution.rs:13, sync)
//   ExecutionParameters::set_fd(ShellFd, OpenFile)       (interp.rs:147)
//   openfiles::OpenFiles::{STDOUT_FD, STDERR_FD}         (openfiles.rs:328/330)
//   Shell::env_str / set_env_global            (shell/env.rs:14/33)
//   Shell::set_working_dir / working_dir       (shell/fs.rs:21 / shell.rs:548)
//   Shell::open_files_mut()                    (shell.rs:484, pub)
//   Shell::last_exit_status() -> u8            (shell.rs:527)
//   ShellVariable::new / export                (variables.rs:68/86)
//   u8::from(result.exit_code)                 (results.rs:177)
use brush_core::openfiles::{OpenFile, OpenFiles};
use brush_core::{ProfileLoadBehavior, RcLoadBehavior, Shell, ShellVariable, SourceInfo};

/// Map a `brush_core::Error` (or any Display error) into a Python exception.
fn to_pyerr<E: std::fmt::Display>(e: E) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// Create a fresh anonymous tempfile to use as a capture target.
fn capture_tempfile() -> PyResult<std::fs::File> {
    tempfile::tempfile().map_err(to_pyerr)
}

/// Rewind a capture tempfile and read its full contents as a lossy UTF-8 string.
fn read_capture(mut f: std::fs::File) -> PyResult<String> {
    f.seek(SeekFrom::Start(0)).map_err(to_pyerr)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(to_pyerr)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Result of running shell code: captured output plus the exit status.
#[pyclass(name = "CompletedCommand", frozen)]
struct CompletedCommand {
    #[pyo3(get)]
    stdout: String,
    #[pyo3(get)]
    stderr: String,
    #[pyo3(get)]
    exit_code: u8,
}

#[pymethods]
impl CompletedCommand {
    /// True when the command exited 0.
    #[getter]
    fn success(&self) -> bool {
        self.exit_code == 0
    }
    fn __bool__(&self) -> bool {
        self.exit_code == 0
    }
    fn __repr__(&self) -> String {
        format!(
            "CompletedCommand(exit_code={}, stdout={:?}, stderr={:?})",
            self.exit_code, self.stdout, self.stderr
        )
    }
}

/// An embedded brush shell. Stateful across calls: variables, exported env, the
/// current working directory, and defined functions all persist on one instance.
///
/// `unsendable`: this object owns a Tokio runtime + a Shell and is not shared across
/// Python threads. (If cross-thread use is later needed, wrap the Shell in a Mutex.)
#[pyclass(name = "Shell", unsendable)]
struct PyShell {
    rt: tokio::runtime::Runtime,
    shell: Shell, // = Shell<DefaultShellExtensions>; the generic is never leaked to Python.
}

#[pymethods]
impl PyShell {
    /// Construct an embedded bash-mode shell.
    ///
    /// * `inherit_env`  - inherit the host process environment (default true).
    /// * `load_rc`      - source the host ~/.bashrc / profile (default false; sandbox-friendly).
    /// * `cwd`          - initial working directory (default: process cwd).
    #[new]
    #[pyo3(signature = (inherit_env = true, load_rc = false, cwd = None))]
    fn new(
        py: Python<'_>,
        inherit_env: bool,
        load_rc: bool,
        cwd: Option<PathBuf>,
    ) -> PyResult<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(to_pyerr)?;

        let (profile, rc) = if load_rc {
            (ProfileLoadBehavior::LoadDefault, RcLoadBehavior::LoadDefault)
        } else {
            (ProfileLoadBehavior::Skip, RcLoadBehavior::Skip)
        };

        let shell = py
            .allow_threads(|| {
                rt.block_on(async {
                    Shell::builder()
                        // MANDATORY: without builtins there is no echo/cd/export/printf.
                        .default_builtins(BuiltinSet::BashMode)
                        .do_not_inherit_env(!inherit_env)
                        // bon generates `maybe_*` setters for Option fields
                        // (working_dir is Option<PathBuf>, builder.rs:190).
                        .maybe_working_dir(cwd)
                        .profile(profile)
                        .rc(rc)
                        .build()
                        .await
                })
            })
            .map_err(to_pyerr)?;

        Ok(Self { rt, shell })
    }

    /// Run a command string, capturing stdout/stderr and returning a `CompletedCommand`.
    ///
    /// REPL-style: does NOT run exit handlers (use this to keep reusing the shell).
    /// If `combine_stderr` is true, stderr is merged into stdout (2>&1) and `.stderr` is empty.
    #[pyo3(signature = (command, combine_stderr = false))]
    fn run(
        &mut self,
        py: Python<'_>,
        command: &str,
        combine_stderr: bool,
    ) -> PyResult<CompletedCommand> {
        let out_file = capture_tempfile()?;
        let err_file = capture_tempfile()?;

        // Clone handles to install into the params; originals are read back after the run.
        let out_for_fd = out_file.try_clone().map_err(to_pyerr)?;
        let err_for_fd = if combine_stderr {
            out_file.try_clone().map_err(to_pyerr)? // point STDERR at the same file as STDOUT
        } else {
            err_file.try_clone().map_err(to_pyerr)?
        };

        let command = command.to_owned();
        let result = py
            .allow_threads(|| {
                // Compute params BEFORE the &mut borrow of run_string (borrow-checker constraint).
                let mut params = self.shell.default_exec_params();
                params.set_fd(OpenFiles::STDOUT_FD, OpenFile::from(out_for_fd));
                params.set_fd(OpenFiles::STDERR_FD, OpenFile::from(err_for_fd));
                self.rt
                    .block_on(self.shell.run_string(command, &SourceInfo::default(), &params))
            })
            .map_err(to_pyerr)?;

        let stdout = read_capture(out_file)?;
        let stderr = if combine_stderr {
            String::new()
        } else {
            read_capture(err_file)?
        };

        Ok(CompletedCommand {
            stdout,
            stderr,
            // Failures (incl. syntax errors -> exit 2, message on stderr) surface via
            // exit_code, not exceptions; the truth is always in exit_code.
            exit_code: u8::from(result.exit_code),
        })
    }

    /// Run a script FILE with positional args ($0 = path, $1.. = args). Performs exit
    /// handling (intended for one-shot execution). Output is captured via the shell's
    /// persistent fd table for the duration of the call, because `run_script` takes no
    /// params arg (execution.rs:213); we install fds on `open_files_mut()` (shell.rs:484, pub).
    #[pyo3(signature = (path, args = None))]
    fn run_script(
        &mut self,
        py: Python<'_>,
        path: PathBuf,
        args: Option<Vec<String>>,
    ) -> PyResult<CompletedCommand> {
        let out_file = capture_tempfile()?;
        let err_file = capture_tempfile()?;
        let out_for_fd = out_file.try_clone().map_err(to_pyerr)?;
        let err_for_fd = err_file.try_clone().map_err(to_pyerr)?;
        let args = args.unwrap_or_default();

        let result = py
            .allow_threads(|| {
                // run_script takes no params, so install fds on the persistent table.
                self.shell
                    .open_files_mut()
                    .set_fd(OpenFiles::STDOUT_FD, OpenFile::from(out_for_fd));
                self.shell
                    .open_files_mut()
                    .set_fd(OpenFiles::STDERR_FD, OpenFile::from(err_for_fd));
                self.rt
                    .block_on(self.shell.run_script(path, args.into_iter()))
            })
            .map_err(to_pyerr)?;

        Ok(CompletedCommand {
            stdout: read_capture(out_file)?,
            stderr: read_capture(err_file)?,
            exit_code: u8::from(result.exit_code),
        })
    }

    /// Set a shell variable. When `export=True` (default) it is exported so spawned
    /// child processes see it (a non-exported var is local to this shell otherwise).
    #[pyo3(signature = (name, value, export = true))]
    fn setenv(&mut self, name: &str, value: &str, export: bool) -> PyResult<()> {
        let mut var = ShellVariable::new(value);
        if export {
            var.export();
        }
        self.shell.set_env_global(name, var).map_err(to_pyerr)
    }

    /// Get a shell/environment variable as a string, or None if unset.
    fn getenv(&self, name: &str) -> Option<String> {
        self.shell.env_str(name).map(|c| c.into_owned())
    }

    /// Change the working directory (the `cd` primitive; updates $PWD/$OLDPWD).
    fn cd(&mut self, path: PathBuf) -> PyResult<()> {
        self.shell.set_working_dir(path).map_err(to_pyerr)
    }

    /// Return the current working directory as a string.
    fn cwd(&self) -> String {
        self.shell.working_dir().display().to_string()
    }

    /// Run a command with `bash -c` semantics: execute the string, then run shell-exit
    /// handling (EXIT traps, etc.). One-shot style; for repeated REPL-style use on the
    /// same instance prefer `run()`. Process-safe: `run_dash_c_command` returns a result
    /// rather than terminating the process (execution.rs:184).
    #[pyo3(signature = (command, combine_stderr = false))]
    fn run_c(
        &mut self,
        py: Python<'_>,
        command: &str,
        combine_stderr: bool,
    ) -> PyResult<CompletedCommand> {
        let out_file = capture_tempfile()?;
        let err_file = capture_tempfile()?;
        let out_for_fd = out_file.try_clone().map_err(to_pyerr)?;
        let err_for_fd = if combine_stderr {
            out_file.try_clone().map_err(to_pyerr)?
        } else {
            err_file.try_clone().map_err(to_pyerr)?
        };
        let command = command.to_owned();

        let result = py
            .allow_threads(|| {
                // run_dash_c_command computes its own params internally, so install the
                // capture fds on the shell's persistent table (inherited by default_exec_params).
                self.shell
                    .open_files_mut()
                    .set_fd(OpenFiles::STDOUT_FD, OpenFile::from(out_for_fd));
                self.shell
                    .open_files_mut()
                    .set_fd(OpenFiles::STDERR_FD, OpenFile::from(err_for_fd));
                self.rt.block_on(self.shell.run_dash_c_command(command))
            })
            .map_err(to_pyerr)?;

        let stdout = read_capture(out_file)?;
        let stderr = if combine_stderr {
            String::new()
        } else {
            read_capture(err_file)?
        };
        Ok(CompletedCommand {
            stdout,
            stderr,
            exit_code: u8::from(result.exit_code),
        })
    }

    /// Invoke a shell function defined in this shell by name, passing string args.
    /// Returns a `CompletedCommand` with captured output and the function's exit status.
    /// Raises if no function with that name is defined (`invoke_function` funcs.rs:93,
    /// which takes execution params, so we capture via `params.set_fd`).
    #[pyo3(signature = (name, args = None, combine_stderr = false))]
    fn call_function(
        &mut self,
        py: Python<'_>,
        name: &str,
        args: Option<Vec<String>>,
        combine_stderr: bool,
    ) -> PyResult<CompletedCommand> {
        let out_file = capture_tempfile()?;
        let err_file = capture_tempfile()?;
        let out_for_fd = out_file.try_clone().map_err(to_pyerr)?;
        let err_for_fd = if combine_stderr {
            out_file.try_clone().map_err(to_pyerr)?
        } else {
            err_file.try_clone().map_err(to_pyerr)?
        };
        let name = name.to_owned();
        let args = args.unwrap_or_default();

        let exit_code = py
            .allow_threads(|| {
                let mut params = self.shell.default_exec_params();
                params.set_fd(OpenFiles::STDOUT_FD, OpenFile::from(out_for_fd));
                params.set_fd(OpenFiles::STDERR_FD, OpenFile::from(err_for_fd));
                self.rt
                    .block_on(self.shell.invoke_function(name, args, &params))
            })
            .map_err(to_pyerr)?;

        let stdout = read_capture(out_file)?;
        let stderr = if combine_stderr {
            String::new()
        } else {
            read_capture(err_file)?
        };
        Ok(CompletedCommand {
            stdout,
            stderr,
            exit_code,
        })
    }

    /// The last exit status recorded by the shell.
    fn last_exit_status(&self) -> u8 {
        self.shell.last_exit_status()
    }
}

/// The compiled `brush._brush` extension module. The pure-Python `brush` package
/// (python/brush/__init__.py) re-exports `Shell` and `CompletedCommand` from here.
#[pymodule]
fn _brush(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyShell>()?;
    m.add_class::<CompletedCommand>()?;
    Ok(())
}
