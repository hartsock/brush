# brush-py

Python bindings for [`brush`](https://github.com/reubeno/brush), an embeddable
POSIX/bash-compatible shell written in Rust. `brush-py` is a thin
[PyO3](https://pyo3.rs) wrapper over the published `brush-core` and
`brush-builtins` crates, exposing a stateful, embeddable shell to Python.

It makes **zero changes** to any existing brush crate: it depends on the
released crates.io versions (`brush-core` 0.5, `brush-builtins` 0.2) and lives in
its own independent cargo workspace, so it is insulated from in-tree API churn.

## What you get

- A persistent `Shell` object: variables, exported environment, the current
  working directory, and shell functions all survive across calls on one
  instance.
- stdout/stderr captured via tempfiles (no 64 KB OS-pipe deadlock), so even
  large output and external commands are captured cleanly.
- `bash`-mode builtins enabled by default (`echo`, `cd`, `export`, `printf`, ...).
- Bundled type stubs (`py.typed`) for editor autocomplete and `mypy`/`pyright`.

## Build & install (development)

Requires a Rust toolchain and Python 3.9+.

```bash
cd brush-py
python3 -m venv .venv && source .venv/bin/activate   # or any venv
pip install maturin
maturin develop            # builds the cdylib and installs it into the active venv
python example.py          # runs the end-to-end demo
```

To build a release wheel instead:

```bash
maturin build --release    # wheel lands in target/wheels/
```

CI builds multi-platform abi3 wheels and publishes to PyPI on
`brush-py-v*` tags; see `.github/workflows/wheels.yml`.

## Usage

```python
import brush

sh = brush.Shell()                       # sandboxed: rc/profile skipped, host env inherited

r = sh.run("echo hello && echo oops >&2; false")
print(r.stdout)        # 'hello\n'
print(r.stderr)        # 'oops\n'
print(r.exit_code)     # 1
print(r.success)       # False  (also: bool(r))

# State persists across calls.
sh.run("x=42")
print(sh.run("echo $x").stdout.strip())  # 42

# Exported environment variables.
sh.setenv("FOO", "bar")                  # export=True by default
print(sh.getenv("FOO"))                  # 'bar'

# Working directory.
sh.cd("/tmp")
print(sh.cwd())                          # '/tmp'

# Combine stderr into stdout (like 2>&1).
r = sh.run("echo out; echo err >&2", combine_stderr=True)

# Define a function, then invoke it directly by name.
sh.run('shout() { echo "$1" | tr a-z A-Z; }')
print(sh.call_function("shout", ["loud"]).stdout.strip())  # 'LOUD'

# Run a script file with positional args.
res = sh.run_script("/path/to/script.sh", ["arg1", "arg2"])
```

### API

`brush.Shell(inherit_env=True, load_rc=False, cwd=None)`

| Method | Description |
| --- | --- |
| `run(command, combine_stderr=False) -> CompletedCommand` | Run a command string (REPL-style; no exit handlers). |
| `run_c(command, combine_stderr=False) -> CompletedCommand` | Run with `bash -c` semantics (runs EXIT traps afterward). |
| `run_script(path, args=[]) -> CompletedCommand` | Run a script file with positional args (runs exit handlers). |
| `call_function(name, args=[], combine_stderr=False) -> CompletedCommand` | Invoke a defined shell function by name (raises if undefined). |
| `setenv(name, value, export=True)` | Set a shell variable, exported by default. |
| `getenv(name) -> Optional[str]` | Read a variable, or `None` if unset. |
| `cd(path)` | Change the working directory. |
| `cwd() -> str` | Current working directory. |
| `last_exit_status() -> int` | Exit status of the last command. |

`CompletedCommand` carries `.stdout`, `.stderr`, `.exit_code`, `.success`, and
supports `bool(...)` (true iff exit code 0).

**Error contract:** syntax/parse errors are reported bash-style — `exit_code == 2`
with the parser message on `.stderr` (they do **not** raise). A `RuntimeError` is
raised only for lower-level execution failures. Check `.success` / `.exit_code`.

The package ships [PEP 561](https://peps.python.org/pep-0561/) type stubs
(`brush/__init__.pyi` + `py.typed`), so editors and `mypy`/`pyright` get full
autocomplete and type checking for the API.

## License

MIT.
