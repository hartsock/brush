# Upstream contribution draft: exec/open interception hook for `ShellExtensions`

> **Status: DRAFT — not yet filed upstream.**
> This document is a prepared issue + PR description for offering the
> capability-confinement hook (implemented in this fork) back to
> [`reubeno/brush`](https://github.com/reubeno/brush). A human will review and
> decide whether and when to file it. Do **not** post this automatically.

---

## Part A — Proposed issue body

### Title

Add an exec/open interception hook to `ShellExtensions` so embedders can confine commands in-process

### Summary

`brush-core` is explicitly designed to be embedded in other Rust programs
(`Shell<SE: ShellExtensions>`, the builder, the `examples/custom-builtin.rs`
guide). A natural and increasingly common reason to embed a shell is to run
*untrusted* or *semi-trusted* scripts — for example, the action plans produced
by an LLM agent — while applying a host-defined policy about which programs may
run and which files may be touched.

Today an embedder cannot reliably enforce such a policy **in-process**. There is
no hook on the path from "the shell decided to run an external command / open a
file" to "the OS call happens." The only extension point on `ShellExtensions` is
`ErrorFormatter`, which is about presentation, not authority.

### The concrete gap

External command dispatch in `brush-core/src/commands.rs`
(`CommandExecutionInfo::execute`) has two branches:

```rust
// brush-core/src/commands.rs (around the resolution logic)
if !sys::fs::contains_path_separator(&self.command_name) {
    // PATH search + builtin table consulted here ...
    if let Some(path) = path {
        self.execute_via_external(&path)
    } else {
        Err(ErrorKind::CommandNotFound(self.command_name).into())
    }
} else {
    // Path-separator branch: PATH and the builtin table are BYPASSED.
    let command_name = PathBuf::from(self.command_name.clone());
    self.execute_via_external(command_name.as_path())
}
```

This is correct bash behavior — a command whose name contains a `/` (e.g.
`/bin/rm`, `./script`, `../x`) is run directly without a PATH search and without
considering builtins. But it means an embedder who tries to confine execution by
inspecting names/PATH cannot stop `/bin/rm`: it skips exactly the machinery they
might hook. Any name-based or PATH-based gate is trivially bypassed by spelling
the command with a path separator.

File opens have the same shape: redirections (`> file`, `< file`, `>> file`) and
`source`/`.` all funnel through `Shell::open_file` in
`brush-core/src/shell/fs.rs`, but there is no hook there either.

### Proposal

Add a new optional component trait to `ShellExtensions` — call it
`CommandInterceptor` — with two hooks that default to "allow," so existing
embedders and the default shell are byte-for-byte unchanged:

```rust
pub enum ExecDecision { Allow, Deny(String) }
pub enum OpenDecision { Allow, Deny(String) }

pub trait CommandInterceptor: Clone + Default + Send + Sync + 'static {
    fn before_exec(&self, program: &str, args: &[String]) -> ExecDecision {
        ExecDecision::Allow
    }
    fn before_open(&self, path: &std::path::Path, write: bool) -> OpenDecision {
        OpenDecision::Allow
    }
}
```

- `before_exec` is called at the **single external-spawn funnel**
  (`execute_external_command`), so **both** dispatch branches — including the
  path-separator branch — are covered. A policy here cannot be circumvented by
  spelling the command differently.
- `before_open` is called inside `Shell::open_file`, the single choke point all
  filesystem-path opens flow through (redirections and `source`/`.`).
- On `Deny`, the operation fails with a clear error (a new
  `ErrorKind::ExecDenied` for execs; a `PermissionDenied` `io::Error` for opens,
  which the existing redirection/source error wrapping already surfaces). It does
  **not** panic and does **not** silently skip.

This mirrors the existing `ErrorFormatter` pattern exactly: a sub-trait collected
as an associated type on `ShellExtensions`, with a `Default*` implementation that
is the no-op/standard behavior.

### Why upstream might want this

- It makes `brush-core` usable as a *sandboxable* embedded shell — a
  differentiator versus shelling out to `bash`.
- It is purely additive and zero-cost when unused (the default impl is inlined to
  nothing).
- It does not commit brush to any particular sandboxing technology (Landlock,
  namespaces, seccomp, brokered FDs); it is the minimal in-process seam those
  approaches can build on.

### Use case that motivated it (agent-bridle)

We are building an object-capability ("ocap") confinement layer for LLM coding
agents. The agent harness is a classic *confused deputy*: it holds the user's
full ambient authority and simultaneously executes untrusted instructions. The
ocap remedy is to hand each agent an *attenuated* capability — "you may run these
programs, you may write under this directory" — and to enforce attenuation at the
point of use. An embedded `brush` is the script-execution surface, so the
enforcement point must be inside the shell, before the spawn/open happens. The
path-separator bypass is precisely the kind of hole that makes name-based
allowlists worthless; `before_exec` at the spawn funnel closes it.

---

## Part B — Proposed pull request description

### Title

feat(core): add `CommandInterceptor` exec/open hook to `ShellExtensions`

### What

Adds an optional `CommandInterceptor` component to `ShellExtensions` with two
default-allow hooks, `before_exec(program, args)` and `before_open(path, write)`,
letting an embedding host deny external command execution and file opens
in-process. Default behavior is unchanged.

### Why

See the issue above. In short: embedders cannot currently confine command
execution in-process, and the path-separator dispatch branch (`/bin/rm`,
`./x`) bypasses PATH and the builtin table, defeating any name/PATH-based gate.
This is the minimal additive seam to close that.

### How

- New sub-trait `extensions::CommandInterceptor` (mirrors `ErrorFormatter`),
  collected as `ShellExtensions::CommandInterceptor`, with
  `DefaultCommandInterceptor` (allow-all). `ShellExtensionsImpl` gains a second
  type parameter that defaults to `DefaultCommandInterceptor`, so
  `DefaultShellExtensions` and all existing call sites are source-compatible.
- `Shell<SE>` stores a `command_interceptor: SE::CommandInterceptor` instance
  (parallel to `error_formatter`), wired through `CreateOptions`/the builder,
  `new`, `Default`, and `Clone`. New accessor `Shell::command_interceptor()`.
- `before_exec` is invoked once, at the top of
  `commands::execute_external_command` — the single funnel for both dispatch
  branches. Denials return a new `ErrorKind::ExecDenied(program, reason)`
  (mapped to exit code 126, "cannot execute").
- `before_open` is invoked in `Shell::open_file` (covers redirections and
  `source`/`.`). Denials return an `io::Error(PermissionDenied)`, which the
  existing `RedirectionFailure` / `FailedSourcingFile` wrapping reports cleanly.
  Write-intent is derived from the `OpenOptions` (best-effort, fail-safe toward
  "write").

### Test plan

- New integration test `brush-core/tests/command_interceptor_tests.rs`:
  - denies a bare-name command (`rm`, PATH-resolved);
  - **denies an absolute-path command (`/bin/rm`)**, proving the
    path-separator branch is now hooked (the load-bearing case);
  - allows a permitted command (`/bin/true`);
  - denies an output redirection writing outside an allowed directory while
    allowing one inside it;
  - allows a read-only `source` open (proving the `write` flag is threaded).
- Full existing suite (`cargo test --workspace`, including the YAML bash-compat
  cases and the redirection cases) passes unchanged — the default-allow impl is
  behavior-preserving.
- `cargo fmt --check` and `cargo clippy --workspace --all-targets -D warnings`
  are clean.

### Compatibility / risk

Additive only. `ShellExtensionsImpl`'s new type parameter is defaulted, so
downstream `type X = ShellExtensionsImpl<MyFormatter>` keeps compiling. No
behavior change for any shell that does not opt in to a custom interceptor.

### Open questions for the maintainer

- Naming: `CommandInterceptor` vs. `SecurityPolicy` vs. `Sandbox`? Hook names
  `before_exec`/`before_open` vs. `check_exec`/`check_open`?
- Should `before_open` get its own `ErrorKind::OpenDenied` rather than reusing a
  `PermissionDenied` `io::Error`? (We chose the latter to fit the existing
  open-file error flow.)
- Would you want the hook to also see the resolved `argv0` / process-group
  policy, or is `(program, args)` sufficient?
- Should there be a parallel async hook, or is sync acceptable given spawns are
  composed synchronously?
