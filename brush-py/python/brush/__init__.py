"""brush - embeddable POSIX/bash shell for Python, backed by the Rust `brush` shell.

The heavy lifting lives in the compiled `_brush` extension (PyO3 over brush-core);
this package re-exports its public API and ships type stubs (see ``__init__.pyi``).

Example::

    import brush
    sh = brush.Shell()
    r = sh.run("echo hello | tr a-z A-Z")
    print(r.stdout, r.exit_code)   # 'HELLO\\n' 0
"""

from ._brush import CompletedCommand, Shell

__all__ = ["Shell", "CompletedCommand"]

try:  # populated from the installed wheel metadata; absent in some dev layouts.
    from importlib.metadata import PackageNotFoundError, version

    try:
        __version__ = version("brush-shell")
    except PackageNotFoundError:  # pragma: no cover
        __version__ = "0.0.0+unknown"
except ImportError:  # pragma: no cover - importlib.metadata always present on 3.9+
    __version__ = "0.0.0+unknown"
