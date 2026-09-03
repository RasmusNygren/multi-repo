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

The project currently targets macOS and Linux and requires Git and Rust 1.95
or newer:

```console
cargo install --locked --path crates/multi-repo-cli
```

## Configure

Each managed collection is a workspace with a `.multi-repo.toml` configuration
at its root. Commands find the nearest configuration by searching the current
directory and its parents, so they also work from inside a managed repository.
Pass `--config PATH` to select a workspace explicitly.

Create a directory for the collection and add `.multi-repo.toml`:

```toml
version = 1

[[source]]
name = "github"
kind = "github"
api_url = "https://api.github.com"
token = "replace-with-your-github-token"
clone_protocol = "ssh"
include = ["*/mar-*"]
include_forks = false
include_archived = false

[[source]]
name = "stash"
kind = "bitbucket-server"
base_url = "https://stash.example.com"
token = "replace-with-your-bitbucket-token"
clone_protocol = "ssh"
projects = ["PLATFORM"]

[[repo]]
id = "special-tool"
url = "git@github.com:acme/special-tool.git"
default_branch = "main"
tags = ["manual"]
```

The configuration directory is the workspace root by default. An optional
`root` setting can select another location; all relative paths, including
`root`, manifests, CA bundles, and local repository URLs, are resolved from the
configuration file.
Different workspace directories can therefore define completely independent
collections that combine any number of GitHub, Bitbucket, and manifest sources.

Repositories declared directly in `.multi-repo.toml` use their configured IDs
and are stored under `<workspace>/repos/<id>`. They can be selected with
`--source workspace` as well as normal `--repo` and `--tag` filters.

Each GitHub or Bitbucket source must configure exactly one of `token` or
`token_env`. An inline `token` is the simplest option, but it is stored as
plaintext in `.multi-repo.toml`. Keep that file local, never commit or share it,
and restrict its permissions on Unix-like systems:

```console
chmod 600 .multi-repo.toml
```

For a shared configuration or an automated environment, store only the name of
an environment variable instead:

```toml
token_env = "GITHUB_TOKEN"
```

Inline tokens are redacted from debug output and neither form is written to the
state database. Bitbucket uses its HTTP access token as a bearer token for REST
discovery. Clone and fetch authentication is delegated to Git, so SSH
configuration and HTTPS credential helpers continue to work.

For large, generated, or reusable repository lists, a workspace can optionally
reference a separate manifest:

```toml
[[source]]
name = "generated"
kind = "manifest"
path = "repos.toml"
tags = ["generated"]
```

The external manifest is also versioned TOML:

```toml
version = 1

[[repo]]
id = "acme/widget"
url = "git@github.com:acme/widget.git"
default_branch = "main"
tags = ["rust", "service"]
```

Local repository URLs in an external manifest are resolved relative to that
manifest file. Remote HTTPS and SSH URLs are used unchanged.

Provider-level `include` and `exclude` values are optional globs over names such
as `acme/widget`. Bitbucket's `projects` list can be omitted to discover all
repositories readable by the token.

## Synchronize

```console
cd ~/workspaces/my-collection
multi-repo sync
multi-repo sync --jobs 4
multi-repo sync --dry-run
multi-repo list
```

Interactive synchronization shows a compact progress bar while sources are
discovered and repositories are updated. The bar is written to the terminal
only and is omitted when output is redirected.

Dry runs compare discovery with the current inventory without changing either
the inventory or working trees. Repositories prefixed with `+` would become
active, repositories prefixed with `-` would become inactive, and the remaining
unchanged count is shown separately:

```text
repository changes:
  + github/acme/new-service
  - github/acme/retired-service
    58 unchanged
```

Additions are green and removals red on an interactive terminal. Use
`--color=always` or `--color=never` to override automatic color detection. A
dry run with no inventory changes prints only `repository changes: none`.

New repositories are cloned with full history and blobs for the default branch.
Existing repositories are fetched and fast-forwarded only when they are clean,
on the recorded default branch, and strictly behind the remote. Dirty,
divergent, detached, and feature-branch working trees are fetched but otherwise
left untouched. Repositories removed from a source become inactive; their local
directories are never automatically deleted.

After reviewing inactive repositories, prune clean working trees with:

```console
multi-repo list --all
multi-repo prune --dry-run
multi-repo prune
```

Prune removes only inactive Git working trees with no tracked changes or
non-ignored untracked files. Dirty or unverifiable directories are retained.
Repositories with local-only branch commits, stashes, or linked worktrees are
also retained. Ignored files do not make a Git working tree dirty. A successful
deletion also removes the repository's inactive inventory entry and any managed
parent directories left empty by the deletion.

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

Managed repositories live under `<workspace>/repos`; the transactional
inventory is stored at `<workspace>/.multi-repo/state.sqlite3`. When `root` is
configured, it replaces `<workspace>` for these paths. Incomplete clones stay
under the private state directory and are made visible only after a successful
atomic rename. Concurrent synchronization is prevented by a workspace lock,
while search remains available.

## Development

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo build --workspace --release --locked
cargo run --release -p multi-repo-core --example search-smoke
```

The next planned layers are tracked-files-only search, isolated batch edits in
Git worktrees, resumable commit/publish runs, and GitHub/Bitbucket pull-request
adapters. A persistent search index is intentionally deferred until real corpus
benchmarks demonstrate that the index-free engine needs one.

## License

MIT
