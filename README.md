# multi-repo

`multi-repo` is a fast, safe manager for collections of Git repositories. It
discovers repositories, keeps local clones up to date without destroying local
work, and searches every working tree in one process.

The initial release supports:

- GitHub and GitHub Enterprise repository discovery.
- Bitbucket Server/Data Center 8.19 repository discovery.
- Explicit TOML repository manifests.
- Concurrent clone/fetch with fast-forward-only working tree updates.
- In-process, parallel, ripgrep-class searching across every repository.

## Install

The project currently targets macOS and Linux and requires Git:

```console
cargo install --path crates/multi-repo-cli
```

## Configure

The default configuration is
`$XDG_CONFIG_HOME/multi-repo/config.toml`. Pass `--config PATH` to use another
file. Relative paths are resolved from the configuration file.

```toml
version = 1
root = "~/src/managed"

[[sources]]
name = "github"
kind = "github"
api_url = "https://api.github.com"
token_env = "GITHUB_TOKEN"
clone_protocol = "ssh"
include_forks = false
include_archived = false

[[sources]]
name = "stash"
kind = "bitbucket-server"
base_url = "https://stash.example.com"
token_env = "BITBUCKET_TOKEN"
clone_protocol = "ssh"
projects = ["PLATFORM"]

[[sources]]
name = "manual"
kind = "manifest"
path = "repos.toml"
tags = ["manual"]
```

Tokens are read only from the named environment variables and are never saved
in the configuration or state database. Bitbucket uses its HTTP access token as
a bearer token for REST discovery. Clone and fetch authentication is delegated
to Git, so SSH configuration and HTTPS credential helpers continue to work.

A manifest is also versioned TOML:

```toml
version = 1

[[repos]]
id = "acme/widget"
url = "git@github.com:acme/widget.git"
default_branch = "main"
tags = ["rust", "service"]
```

Provider-level `include` and `exclude` values are optional globs over names such
as `acme/widget`. Bitbucket's `projects` list can be omitted to discover all
repositories readable by the token.

## Synchronize

```console
multi-repo sync
multi-repo sync --jobs 4
multi-repo sync --dry-run
multi-repo list
```

New repositories are cloned with full history and blobs for the default branch.
Existing repositories are fetched and fast-forwarded only when they are clean,
on the recorded default branch, and strictly behind the remote. Dirty,
divergent, detached, and feature-branch working trees are fetched but otherwise
left untouched. Repositories removed from a source become inactive; their local
directories are never automatically deleted.

## Search

```console
multi-repo grep 'unsafe\s*\{'
multi-repo grep -F 'edition = "2024"' -g 'Cargo.toml'
multi-repo grep pre-commit --repos-with-matches
multi-repo grep TODO --source github --repo 'github/acme/*'
multi-repo grep error -C 2 --json
multi-repo grep needle -- src tests
```

Search runs directly through ripgrep's Rust libraries. It does not launch one
`git grep` or `rg` process per repository. The default file universe contains
tracked files and non-ignored untracked files, includes dotfiles such as
`.github/workflows`, respects Git and ripgrep ignore files, skips binary files,
does not follow symlinks, and always excludes `.git`.

Useful modes include fixed strings (`-F`), case control (`-i`/`-S`), word
matching (`-w`), path globs (`-g`), context (`-A`/`-B`/`-C`), file or repository
names only, counts, JSON Lines, and deterministic `--sort-path` output. Normal
results use `repository:path:line:column:text`. Exit status is 0 for matches, 1
for no matches, and 2 for errors.

## State and safety

Managed repositories live under `<root>/repos`; the transactional inventory is
stored at `<root>/.multi-repo/state.sqlite3`. Incomplete clones stay under the
private state directory and are made visible only after a successful atomic
rename. Concurrent synchronization is prevented by a workspace lock, while
search remains available.

## Development

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --workspace --release
cargo run --release -p multi-repo-core --example search-smoke
```

The next planned layers are tracked-files-only search, isolated batch edits in
Git worktrees, resumable commit/publish runs, and GitHub/Bitbucket pull-request
adapters. A persistent search index is intentionally deferred until real corpus
benchmarks demonstrate that the index-free engine needs one.

## License

MIT
