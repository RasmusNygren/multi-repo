# Development

## Prerequisites

Install Git and Rust 1.95 or newer. Clone the repository, then build the
workspace:

```console
git clone https://github.com/RasmusNygren/multi-repo.git
cd multi-repo
cargo build --workspace --locked
```

Run the CLI without installing it:

```console
cargo run -p multi-repo -- --help
```

## Validation

Run the same checks expected by continuous integration before submitting a
change:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo test --workspace --all-targets --locked
cargo build --workspace --release --locked
cargo package --workspace --locked
```

The search smoke test can also be run against a configured workspace:

```console
cargo run --release -p multi-repo-core --example search-smoke
```

## Repository layout

- `crates/multi-repo-cli` contains command-line parsing and output formatting.
- `crates/multi-repo-core` contains configuration, provider discovery, state,
  Git synchronization, pruning, and search.
- `examples` contains example workspace and manifest configuration files.
- `.github/workflows/ci.yml` defines the continuous integration checks.

## Testing changes

Unit tests live alongside their modules. End-to-end command-line tests live in
`crates/multi-repo-cli/tests/e2e.rs`.

When behavior or configuration changes, update its tests, the configuration
reference in `README.md`, and the files under `examples/` in the same change.

## Publishing

### Binary releases

[`cargo-dist`](https://axodotdev.github.io/cargo-dist/) builds the `multi-repo`
binary for macOS (Intel and Apple Silicon) and Linux (x86-64 and ARM64, using
static musl builds). The generated `.github/workflows/release.yml` publishes
archives, SHA-256 checksums, a shell installer, and source code to GitHub
Releases only when manually triggered through **Actions → Release → Run
workflow**. Pushing branches or tags does not publish. Binary archives have
GitHub artifact attestations. Pull requests only validate the release plan;
they do not publish.
This workflow is independent of crates.io and needs no additional secrets
beyond the automatic `GITHUB_TOKEN`.

Install the pinned generator when changing release configuration:

```console
cargo install cargo-dist --version 0.32.0 --locked
```

Edit `dist-workspace.toml`, then regenerate the workflow instead of editing it
directly. Keep all GitHub Actions pinned to full commit hashes in
`dist.github-action-commits`.

```console
dist generate
dist generate --check
dist plan
dist build
```

`dist build` builds for the local host and writes artifacts to `target/distrib/`
without publishing. The full target matrix runs on GitHub when releasing.

To release:

1. Update `workspace.package.version` in `Cargo.toml` and the CLI's
   `multi-repo-core` dependency version together. Run `cargo check --workspace`
   to update `Cargo.lock`, then run the validation checks above.
2. Run `dist plan --tag v0.1.0`, substituting the exact version being released.
   Commit the version changes and any regenerated workflow, push, and wait for
   CI to pass on that commit.
3. Open **Actions → Release → Run workflow**, select the branch containing the
   tested commit, and set **Release Tag** to `v0.1.0` (or the version being
   released). Click **Run workflow** to build and publish. The workflow builds
   the selected branch's commit; the tag input names the release, not the source
   revision. It creates the tag if it does not already exist. If using an
   existing tag, ensure it points to that same commit.

The workflow must be present on the default branch for **Run workflow** to
appear. Leave **Release Tag** at its default `dry-run` to build and upload
workflow artifacts for all four platforms without creating a tag or publishing
a GitHub Release.

The tag version must match the workspace version. Tags such as
`v0.2.0-rc.1` produce prereleases and require that same prerelease version in
the Cargo manifests. Watch the Release workflow for completion; it creates the
release after the builds succeed. Do not pre-create a release for the tag.

To verify a downloaded binary archive's provenance:

```console
gh attestation verify multi-repo-aarch64-apple-darwin.tar.gz --repo RasmusNygren/multi-repo
```

### crates.io

Keep the workspace version and the CLI's `multi-repo-core` dependency version
in sync. After running the validation checks, verify and publish the core crate
first:

```console
cargo publish -p multi-repo-core --locked --dry-run
cargo publish -p multi-repo-core --locked
```

Once that version is available on crates.io, verify and publish the CLI:

```console
cargo publish -p multi-repo --locked --dry-run
cargo publish -p multi-repo --locked
```

The CLI's packaged dependency resolves through crates.io, so its standalone
packaging and publish dry run require the core version to be available there.
