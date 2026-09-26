# Releasing the OCAP filter fork

This guide releases the library packages from
[hartsock/brush](https://github.com/hartsock/brush). The fork follows the static
`CmdExecFilter` / `SourceFilter` architecture proposed in
[upstream PR #1314](https://github.com/reubeno/brush/pull/1314), with the behavior
needed by capability-confined embedders. Fork publication, Newt integration,
and an upstream contribution are separate steps, in that order.

This is a release procedure, not a record that its checks or uploads have run.
Record the actual release commit, package checksums, test results, registry
versions, and GitHub release URLs when performing it.

## Package set

| Package | Candidate version | Rust library name | Fork dependencies |
| --- | --- | --- | --- |
| `brush-ocap-parser` | `0.5.0-rc.1` | `brush_parser` | None |
| `brush-ocap-core` | `0.6.0-rc.1` | `brush_core` | Parser |
| `brush-ocap-builtins` | `0.3.0-rc.1` | `brush_builtins` | Core, parser |
| `brush-ocap-coreutils-builtins` | `0.2.0-rc.1` | `brush_coreutils_builtins` | None |

The parser must travel with this release: the current AST places extended tests
in `CompoundCommand`, while the published upstream parser 0.4.0 places them in
`Command`. A successful workspace build with path dependencies does not prove
that the published core will resolve a compatible parser.

The manifests use these four package names and versions while preserving the
Rust library names. Dependency aliases throughout the workspace keep existing
import and feature names stable. For example, the core's parser dependency is:

```toml
brush-parser = { package = "brush-ocap-parser", version = "=0.5.0-rc.1", path = "../brush-parser" }
```

This candidate's other fork dependencies also use exact prerelease versions.
Repository metadata points to the fork, and the shared README describes the
filter API and migration from the former interceptor API. Application,
interactive, experimental, test, fuzz, and xtask packages are marked
`publish = false`; they remain workspace consumers outside this library release.

## Validate the distributable packages

Use a clean, committed release candidate with a reviewed lockfile. Run the
repository's normal `cargo xtask ci full` gate, including the new filter,
terminating-refusal, file-open, serialization, and execution-contract tests.
Record any unavailable platform or external check explicitly. Cross-platform
claims need Linux, macOS, and Windows evidence; a host-only pass is not that
evidence.

Select the four packages explicitly when checking the archives:

```sh
cargo package --locked --registry crates-io \
  -p brush-ocap-parser@0.5.0-rc.1 \
  -p brush-ocap-core@0.6.0-rc.1 \
  -p brush-ocap-builtins@0.3.0-rc.1 \
  -p brush-ocap-coreutils-builtins@0.2.0-rc.1 \
  --target-dir target/fork-release
```

Cargo verifies the extracted archives. Inspect each archive's normalized
`Cargo.toml`, README, license, and `.cargo_vcs_info.json`. Confirm the fork
package identities and prerelease dependency requirements, the intended commit,
and the absence of local source overrides. Preserve archive checksums with the
release evidence. Exercise `serde` and the carried coreutils feature set used
by Newt in addition to default-feature checks.

Packaging several interdependent crates together allows Cargo to prepare their
dependency lockfiles for the selected registry. If the installed Cargo cannot
verify the unpublished dependency set, resolve that with a compatible Cargo or
a staging registry before uploading; do not replace verification with
`--no-verify`. See the [Cargo packaging reference](https://doc.rust-lang.org/cargo/commands/cargo-package.html).

## Publish the fork

Authenticate through Cargo's credential provider or interactive `cargo login`.
Check the publisher identity and package permissions without displaying tokens
or putting them in command arguments, logs, or this repository. The first parser
publication creates a new package; confirm that the publishing account can do
that as well as update the existing three fork packages.

Upload from the exact validated commit in this order, waiting for each version
to become available in the crates.io index before its dependent package:

```sh
cargo publish --locked --registry crates-io -p brush-ocap-parser@0.5.0-rc.1
cargo publish --locked --registry crates-io -p brush-ocap-core@0.6.0-rc.1
cargo publish --locked --registry crates-io -p brush-ocap-builtins@0.3.0-rc.1
cargo publish --locked --registry crates-io -p brush-ocap-coreutils-builtins@0.2.0-rc.1
```

Coreutils is independent of the other three; placing it last keeps the release
record consistent. Do not publish a workspace-wide wildcard. Cargo does not
read `release-plz.toml`, so explicit package selection also matters for manual
uploads. Do not use `--allow-dirty` or `--no-verify` for the release uploads.

For subsequent releases, `release-plz.toml` disables processing, registry
publication, and tags by default, then enables only the four fork package
names. It targets `hartsock/brush`, uses package-prefixed tags, and marks
prereleases from their versions. GitHub releases start as drafts, following
the existing maintainer workflow; those drafts do not make a crates.io upload
private. See the [release-plz configuration reference](https://release-plz.dev/docs/config).

After manual uploads, create the corresponding
`<package>-v<version>` tags at the validated commit and GitHub prereleases in
`hartsock/brush`, with the test results and archive checksums. For release-plz
uploads, review and publish its generated drafts. Keep prereleases out of the
GitHub latest-release designation. The inherited binary distribution workflow
handles upstream `brush-shell` releases and is not this library publication
path.

## Prove Newt against the published fork

Resolve all four versions from the registry in a fresh consumer build with no
path or git patches for Brush. Update Bridle's direct parser dependency as well
as its core and builtin dependencies: filter signatures must share one parser
package identity. Keep fork prerelease requirements explicit in the downstream
manifest and lockfile.

Run the real Newt worker fixtures for native Cargo pipelines and formatting,
pre-execution permission denial with no effects, sibling filesystem denial,
complete retained output within the capture limit, explicit capture-loss
reporting above it, and scratch ownership through completion and cancellation.
Then rebuild/install Newt and rerun the Lab and Knowledge assignments using the
normal operator permissions and prompts. Record the installed source identity
and results separately from library tests.

Only after that proof, prepare the upstream contribution. Submit the reusable
filter behavior and tests against `reubeno/brush`; keep fork package names,
prerelease bookkeeping, and Newt-specific integration out of that change.
Do not claim an upstream merge or a released upstream replacement until each
has happened. Retire the fork dependency only after the upstream release
provides the required behavior and passes the same consumer tests.
