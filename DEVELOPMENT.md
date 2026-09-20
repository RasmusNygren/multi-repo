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
