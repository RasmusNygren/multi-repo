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

Run the search smoke benchmark on synthetic repositories:

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

### Release configuration

Binary releases use `cargo-dist`, configured in `dist-workspace.toml`. When
changing it, install the pinned generator and regenerate the workflow:

```console
cargo install cargo-dist --version 0.32.0 --locked
dist generate
dist generate --check
dist plan
dist build
```

Keep action hashes in `dist.github-action-commits`; do not edit the generated
workflow directly. `dist build` writes local-host artifacts to `target/distrib/`
without publishing.

### Binary releases

1. Update `workspace.package.version` and the CLI's `multi-repo-core`
   dependency version together. Run `cargo check --workspace` to update
   `Cargo.lock`, then run the validation checks above.
2. Run `dist plan --tag v0.1.0`, using the release version. Commit, push, and
   wait for CI to pass on that commit.
3. Open **Actions → Release → Run workflow**, select the tested branch, and
   set **Release Tag** to the matching version. The workflow must be on the
   default branch for this control to appear.

The workflow releases the selected branch's commit. It creates the tag if
missing; an existing tag must point to that commit. Prerelease tags such as
`v0.2.0-rc.1` require the same prerelease version in Cargo. The workflow creates
the GitHub Release after successful builds; do not pre-create it.

Use **Release Tag** `dry-run` to build all four targets without publishing:
macOS and Linux on x86-64 and ARM64 (Linux uses static musl builds). Releases
include archives, checksums, a shell installer, source, and binary attestations.
Verify an archive with:

```console
gh attestation verify multi-repo-aarch64-apple-darwin.tar.gz --repo RasmusNygren/multi-repo
```

Only manual dispatch publishes; pushes do not, and pull requests only check
the plan. Binary releases use the automatic `GITHUB_TOKEN` and are independent
of crates.io.

### crates.io

After updating versions and running validation as above, publish the core first:

```console
cargo publish -p multi-repo-core --locked --dry-run
cargo publish -p multi-repo-core --locked
```

Wait until that version is available on crates.io before verifying and
publishing the CLI, whose packaged dependency resolves through the registry:

```console
cargo publish -p multi-repo --locked --dry-run
cargo publish -p multi-repo --locked
```
