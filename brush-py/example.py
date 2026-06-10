#!/usr/bin/env python3
"""Example usage of the brush Python bindings.

Build + install locally first:
    cd brush-py
    maturin develop          # or: pip install -e .

Then run:
    python example.py
"""

import brush


def main() -> None:
    # Construct once; state (vars, exported env, cwd, defined functions) persists
    # across calls on this instance. rc/profile are skipped by default (sandboxed),
    # and the host environment is inherited by default.
    sh = brush.Shell()

    # 1) Run a command string, capture stdout/stderr + exit code.
    r = sh.run("echo hello && echo oops >&2; false")
    print("exit_code:", r.exit_code)   # 1
    print("stdout:", repr(r.stdout))   # 'hello\n'
    print("stderr:", repr(r.stderr))   # 'oops\n'
    print("success:", r.success)       # False
    print("bool:", bool(r))            # False

    # 2) State persists across calls.
    sh.run("x=42")
    print("x =>", sh.run("echo $x").stdout.strip())   # 42

    # 3) Define a function in one call, use it in the next.
    sh.run('greet() { echo "Hi, $1"; }')
    print(sh.run("greet Ada").stdout.strip())          # Hi, Ada

    # 4) Environment variables (exported by default, so children see them).
    sh.setenv("FOO", "bar")
    print("getenv FOO:", sh.getenv("FOO"))             # bar
    print("getenv MISSING:", sh.getenv("MISSING"))     # None
    # External command sees the exported var:
    print(sh.run("env | grep '^FOO='").stdout.strip()) # FOO=bar

    # 5) Working directory.
    sh.cd("/tmp")
    print("cwd:", sh.cwd())                            # /tmp
    print("pwd:", sh.run("pwd").stdout.strip())        # /tmp

    # 6) Combined (ordered) output, like 2>&1.
    r = sh.run("echo out; echo err >&2", combine_stderr=True)
    print("combined:", repr(r.stdout))                 # interleaved; r.stderr == ''

    # 7) Large output does NOT deadlock (tempfile-backed capture, not a 64KB pipe).
    big = sh.run("for i in $(seq 1 50000); do echo line $i; done")
    print("big lines:", big.stdout.count("\n"))        # 50000

    # 8) Run a script file (uncomment with a real path):
    # res = sh.run_script("/path/to/script.sh", ["arg1", "arg2"])
    # print("script exit:", res.exit_code)


if __name__ == "__main__":
    main()
