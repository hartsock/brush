"""Type stubs for the `brush` Python bindings (PyO3 over brush-core).

These describe the public API re-exported from the compiled `_brush` extension.
`py.typed` marks the package as typed so checkers (mypy/pyright) use these stubs.
"""

from __future__ import annotations

from os import PathLike
from typing import Optional, Sequence, Union

__version__: str
__all__: list[str]

_StrPath = Union[str, PathLike[str]]

class CompletedCommand:
    """Result of running shell code: captured output plus the exit status."""

    @property
    def stdout(self) -> str:
        """Captured standard output (lossy UTF-8)."""
        ...
    @property
    def stderr(self) -> str:
        """Captured standard error (lossy UTF-8). Empty when ``combine_stderr=True``."""
        ...
    @property
    def exit_code(self) -> int:
        """Process exit status in 0..=255."""
        ...
    @property
    def success(self) -> bool:
        """``True`` when ``exit_code == 0``."""
        ...
    def __bool__(self) -> bool: ...
    def __repr__(self) -> str: ...

class Shell:
    """An embedded brush shell.

    State (variables, exported env, working directory, defined functions) persists
    across calls on a single instance. Not shared across Python threads.
    """

    def __init__(
        self,
        inherit_env: bool = ...,
        load_rc: bool = ...,
        cwd: Optional[_StrPath] = ...,
    ) -> None:
        """Construct a bash-mode shell.

        :param inherit_env: inherit the host process environment (default ``True``).
        :param load_rc: source the host ~/.bashrc / profile (default ``False``).
        :param cwd: initial working directory (default: process cwd).
        """
        ...
    def run(self, command: str, combine_stderr: bool = ...) -> CompletedCommand:
        """Run a command string (REPL-style; no exit handlers), capturing output.

        Syntax/parse errors are reported bash-style: ``exit_code == 2`` with the parser
        message on ``stderr`` (they do NOT raise). ``RuntimeError`` is raised only for
        lower-level execution failures. Check ``.success`` / ``.exit_code``.
        """
        ...
    def run_c(self, command: str, combine_stderr: bool = ...) -> CompletedCommand:
        """Run a command with ``bash -c`` semantics (runs EXIT traps afterward)."""
        ...
    def run_script(
        self, path: _StrPath, args: Optional[Sequence[str]] = ...
    ) -> CompletedCommand:
        """Run a script file with positional args ($0 = path, $1.. = args)."""
        ...
    def call_function(
        self,
        name: str,
        args: Optional[Sequence[str]] = ...,
        combine_stderr: bool = ...,
    ) -> CompletedCommand:
        """Invoke a defined shell function by name. Raises if undefined."""
        ...
    def setenv(self, name: str, value: str, export: bool = ...) -> None:
        """Set a shell variable; exported (visible to children) by default."""
        ...
    def getenv(self, name: str) -> Optional[str]:
        """Get a shell/environment variable, or ``None`` if unset."""
        ...
    def cd(self, path: _StrPath) -> None:
        """Change the working directory (updates $PWD/$OLDPWD)."""
        ...
    def cwd(self) -> str:
        """Return the current working directory."""
        ...
    def last_exit_status(self) -> int:
        """The last exit status recorded by the shell."""
        ...
