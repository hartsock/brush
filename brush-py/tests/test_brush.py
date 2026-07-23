"""Regression tests for the brush Python bindings.

Run from brush-py/ after installing the module (``maturin develop`` or
``pip install <wheel>``):

    pytest -q

These exercise the same scenarios as example.py plus the extra API surface
(run_c, call_function) and the error contracts (parse error -> exception,
undefined function -> exception).
"""

import os

import brush
import pytest


def test_run_echo_captures_stdout_and_success():
    sh = brush.Shell()
    r = sh.run("echo hello")
    assert r.stdout == "hello\n"
    assert r.stderr == ""
    assert r.exit_code == 0
    assert r.success is True
    assert bool(r) is True


def test_nonzero_exit_code():
    sh = brush.Shell()
    r = sh.run("exit 3")
    assert r.exit_code == 3
    assert r.success is False
    assert bool(r) is False


def test_stderr_captured_separately():
    sh = brush.Shell()
    r = sh.run("echo oops >&2")
    assert r.stdout == ""
    assert r.stderr == "oops\n"


def test_combine_stderr_merges_into_stdout():
    sh = brush.Shell()
    r = sh.run("echo out; echo err >&2", combine_stderr=True)
    assert r.stdout == "out\nerr\n"
    assert r.stderr == ""


def test_state_persists_across_calls():
    sh = brush.Shell()
    sh.run("x=42")
    assert sh.run("echo $x").stdout == "42\n"


def test_setenv_getenv_roundtrip():
    sh = brush.Shell()
    sh.setenv("FOO", "bar")
    assert sh.getenv("FOO") == "bar"


def test_getenv_missing_returns_none():
    sh = brush.Shell()
    assert sh.getenv("DEFINITELY_NOT_SET_12345") is None


def test_exported_env_visible_to_external_command():
    sh = brush.Shell()
    sh.setenv("FOO", "bar")  # exported by default
    out = sh.run("env | grep '^FOO='").stdout
    assert "FOO=bar" in out


def test_non_exported_var_not_in_child_env():
    sh = brush.Shell()
    sh.setenv("SECRET", "s3cr3t", export=False)
    assert sh.getenv("SECRET") == "s3cr3t"  # visible to the shell itself
    out = sh.run("env | grep '^SECRET=' || true").stdout
    assert "SECRET=" not in out  # but not exported to children


def test_cd_and_cwd():
    sh = brush.Shell()
    sh.cd("/tmp")
    assert sh.cwd() == "/tmp"
    assert sh.run("pwd").stdout == "/tmp\n"


def test_constructor_cwd():
    sh = brush.Shell(cwd="/tmp")
    assert sh.cwd() == "/tmp"


def test_inherit_env_toggle():
    os.environ["BRUSH_TEST_HOSTVAR"] = "present"
    try:
        assert brush.Shell(inherit_env=True).getenv("BRUSH_TEST_HOSTVAR") == "present"
        assert brush.Shell(inherit_env=False).getenv("BRUSH_TEST_HOSTVAR") is None
    finally:
        del os.environ["BRUSH_TEST_HOSTVAR"]


def test_large_output_no_pipe_deadlock():
    sh = brush.Shell()
    r = sh.run("for i in $(seq 1 50000); do echo line $i; done")
    assert r.stdout.count("\n") == 50000
    assert r.exit_code == 0


def test_run_c_semantics():
    sh = brush.Shell()
    r = sh.run_c("echo via-dash-c")
    assert r.stdout == "via-dash-c\n"
    assert r.exit_code == 0


def test_call_function():
    sh = brush.Shell()
    sh.run('greet() { echo "Hi, $1"; }')
    r = sh.call_function("greet", ["Ada"])
    assert r.stdout == "Hi, Ada\n"
    assert r.exit_code == 0


def test_call_undefined_function_raises():
    sh = brush.Shell()
    with pytest.raises(Exception):
        sh.call_function("no_such_function_xyz")


def test_run_script_file(tmp_path):
    script = tmp_path / "hello.sh"
    script.write_text('echo "script: $1 $2"\nexit 7\n')
    sh = brush.Shell()
    r = sh.run_script(str(script), ["a", "b"])
    assert r.stdout == "script: a b\n"
    assert r.exit_code == 7


def test_syntax_error_reported_bash_style():
    """brush reports syntax errors like bash: exit 2 + message on stderr, NOT an
    exception (verified against brush-core 0.5.0; the Rust API returns Ok here)."""
    sh = brush.Shell()
    r = sh.run("echo $(")  # unterminated command substitution
    assert r.exit_code == 2
    assert r.success is False
    assert "error" in r.stderr.lower()
    # The shell instance stays usable after a syntax error.
    assert sh.run("echo ok").stdout == "ok\n"
