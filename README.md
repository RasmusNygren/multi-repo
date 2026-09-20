# multi-repo

`multi-repo` is a command-line tool for keeping a collection of Git
repositories synchronized and searching all of them at once.

It can:

- discover repositories from GitHub, GitHub Enterprise, Bitbucket Server/Data
  Center 8.19, or a TOML manifest;
- clone, fetch, and safely fast-forward repositories without overwriting local
  work;
- automatically identify Python, Go, and Rust repositories; and
- search every repository in parallel with ripgrep-compatible matching.

`multi-repo` supports macOS and Linux.

## Install

Install [Git](https://git-scm.com/) and Rust 1.95 or newer, then install the
latest version from this repository:

```console
cargo install --locked --git https://github.com/RasmusNygren/multi-repo multi-repo
```

## Quick start

Create a directory for the workspace:

```console
mkdir -p ~/multi-repo-workspace
cd ~/multi-repo-workspace
```

Create `.multi-repo.toml`, replacing the token and organization with your own:

```toml
[[source]]
name = "github"
kind = "github"
token = "your-github-token"
include = ["your-org/*"]
```

The token must be able to list the repositories you want to manage.
Alternatively, [load the token from an environment variable](#credentials).

Synchronize the workspace and search it:

```console
multi-repo sync
multi-repo list
multi-repo grep 'TODO|FIXME'
```

Repositories are cloned into `repos/`. Authentication for cloning and fetching
is handled by Git, independently of the API token used for discovery. The
default clone protocol is SSH, so make sure your SSH key works with GitHub or
set `clone_protocol = "https"` and configure a Git credential helper.

## Common tasks

Preview changes to the repository inventory without modifying state or working
trees:

```console
multi-repo sync --dry-run
```

Search selected repositories, tags, or paths:

```console
multi-repo grep TODO --repo 'github/your-org/*'
multi-repo grep -F 'edition = "2024"' --tag rust -g Cargo.toml
multi-repo grep error -C 2 -- src tests
```

Every successful sync detects Python, Go, and Rust projects from files tracked
on the fetched default branch. Detected repositories receive `language:python`,
`language:go`, or `language:rust` tags and can be selected like any explicitly
configured tag:

```console
multi-repo list --tag language:python
multi-repo grep TODO --tag language:go
```

A repository can receive more than one language tag. Dependency directories
such as `vendor`, `.venv`, and `target` do not affect detection. Each language
requires both a project marker and a matching source file: `go.mod` and `.go`,
Python project metadata and `.py`, or `Cargo.toml` and `.rs`.

Review repositories that disappeared from their source, then remove those that
are safe to delete:

```console
multi-repo list --all
multi-repo prune --dry-run
multi-repo prune
```

Synchronization always fetches existing repositories. It fast-forwards the
checked-out branch only when the working tree is clean, the checked-out branch
is the recorded default branch, and that branch is strictly behind its remote.
Dirty, divergent, detached, and feature-branch working trees are left as they
are.

Repositories that disappear from a source become inactive but are not deleted
by `sync`. `prune` deletes only inactive Git working trees with no tracked or
non-ignored untracked changes, local-only branch commits, stashes, or linked
worktrees.

Pruning also preserves local-only commits on a detached HEAD and rejects
repository paths that resolve outside `repos/`. Removing a source from the
configuration deactivates its repositories unless another source still keeps
them active. A failed discovery retains that source's existing inventory.

## Configuration reference

Commands look for the nearest `.multi-repo.toml` in the current directory or
one of its parents. Use the global `--config PATH` option to select a different
file.

Unknown settings are rejected. Relative paths and `~/` paths are resolved from
the directory containing the configuration file. Unless `root` is set, that
directory is also the workspace root.

### Workspace settings

| Setting | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `root` | path | no | configuration directory | Directory that contains `repos/` and `.multi-repo/`. |
| `[[source]]` | array of tables | no | `[]` | Repository discovery sources. See the source types below. |
| `[[repo]]` | array of tables | no | `[]` | Repositories declared directly in the workspace configuration. |

Source names must be unique, must be a single safe path component, and cannot
be `workspace`, which is reserved for directly declared repositories.

Each source can opt into fetching every branch. If multiple active sources
discover the same repository, all branches are fetched when any associated
source enables the setting. Repositories declared directly with `[[repo]]`
fetch only their default branch.

To fetch all branches for a source, add this setting to its `[[source]]` table:

```toml
[[source]]
name = "catalog"
kind = "manifest"
path = "repos.toml"
fetch_all_branches = true
```

The next `multi-repo sync` fetches all remote branches as `origin/<branch>`
and prunes remote-tracking branches deleted from `origin`. It still only
fast-forwards a clean, checked-out default branch; local feature branches
are not switched or updated.

### GitHub source

```toml
[[source]]
name = "github"
kind = "github"
fetch_all_branches = false
api_url = "https://api.github.com"
token_env = "GITHUB_TOKEN"
clone_protocol = "ssh"
include = ["acme/*"]
exclude = ["acme/archived-*"]
include_forks = false
include_archived = false
include_private = true
tags = ["github"]
```

| Setting | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `name` | string | yes | — | Unique source name. Repository IDs begin with this value. |
| `kind` | string | yes | — | Must be `"github"`. |
| `fetch_all_branches` | boolean | no | `false` | Clone and fetch all branches from `origin`, including for existing single-branch clones. |
| `api_url` | URL | no | `"https://api.github.com"` | REST API base URL. Set this for GitHub Enterprise. |
| `token` | string | conditionally | — | Inline API token. Exactly one of `token` and `token_env` is required. |
| `token_env` | string | conditionally | — | Name of an environment variable containing the API token. Exactly one of `token` and `token_env` is required. |
| `clone_protocol` | string | no | `"ssh"` | Clone URL to use: `"ssh"` or `"https"`. |
| `include` | array of globs | no | `[]` | Include matching `owner/repository` names. An empty list includes all names. |
| `exclude` | array of globs | no | `[]` | Exclude matching `owner/repository` names after applying `include`. |
| `include_forks` | boolean | no | `false` | Include forked repositories. |
| `include_archived` | boolean | no | `false` | Include archived repositories. |
| `include_private` | boolean | no | `true` | Include private repositories. |
| `tags` | array of strings | no | `[]` | Tags added to every repository discovered by this source. |

GitHub repositories receive IDs in the form
`<source-name>/<owner>/<repository>`.

### Bitbucket Server source

```toml
[[source]]
name = "bitbucket"
kind = "bitbucket-server"
fetch_all_branches = false
base_url = "https://bitbucket.example.com"
token_env = "BITBUCKET_TOKEN"
clone_protocol = "ssh"
projects = ["PLATFORM"]
include = ["PLATFORM/*"]
exclude = []
include_archived = false
tags = ["internal"]
ca_bundle = "certificates/company-ca.pem"
```

| Setting | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `name` | string | yes | — | Unique source name. Repository IDs begin with this value. |
| `kind` | string | yes | — | Must be `"bitbucket-server"`. |
| `fetch_all_branches` | boolean | no | `false` | Clone and fetch all branches from `origin`, including for existing single-branch clones. |
| `base_url` | URL | yes | — | Bitbucket Server or Data Center base URL. |
| `token` | string | conditionally | — | Inline HTTP access token used as a bearer token. Exactly one of `token` and `token_env` is required. |
| `token_env` | string | conditionally | — | Name of an environment variable containing the HTTP access token. Exactly one of `token` and `token_env` is required. |
| `clone_protocol` | string | no | `"ssh"` | Clone URL to use: `"ssh"` or `"https"`. |
| `projects` | array of strings | no | `[]` | Project keys to query. An empty list discovers every readable repository. |
| `include` | array of globs | no | `[]` | Include matching `PROJECT/repository` names. An empty list includes all names. |
| `exclude` | array of globs | no | `[]` | Exclude matching `PROJECT/repository` names after applying `include`. |
| `include_archived` | boolean | no | `false` | Include archived repositories. |
| `tags` | array of strings | no | `[]` | Tags added to every repository discovered by this source. |
| `ca_bundle` | path | no | system trust store | PEM-encoded CA certificate to add when connecting to the Bitbucket API. |

Bitbucket repositories receive IDs in the form
`<source-name>/<project>/<repository>`.

### Manifest source

Use a manifest only when the repository list is generated or shared separately
from the workspace configuration. For a hand-maintained list, put `[[repo]]`
entries directly in `.multi-repo.toml` instead.

```toml
[[source]]
name = "catalog"
kind = "manifest"
path = "repos.toml"
fetch_all_branches = false
tags = ["catalog"]
```

| Setting | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `name` | string | yes | — | Unique source name. Repository IDs begin with this value. |
| `kind` | string | yes | — | Must be `"manifest"`. |
| `fetch_all_branches` | boolean | no | `false` | Clone and fetch all branches from `origin`, including for existing single-branch clones. |
| `path` | path | yes | — | Path to the TOML manifest. |
| `tags` | array of strings | no | `[]` | Tags added to every repository in the manifest. |

The manifest uses the same `[[repo]]` format as the workspace configuration:

```toml
[[repo]]
id = "acme/widget"
url = "git@github.com:acme/widget.git"
default_branch = "main"
tags = ["rust", "service"]
```

Manifest repository IDs are prefixed with the source name. Relative local
repository URLs in a manifest are resolved from the manifest's directory.

### Repository settings

Repositories may be declared directly in `.multi-repo.toml` or inside a
manifest.

| Setting | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `id` | string | yes | — | Unique, safe relative path used as the repository ID. Empty, absolute, current-directory, parent-directory, and backslash components are rejected. |
| `url` | string | yes | — | SSH URL, HTTPS URL, or local repository path passed to Git. |
| `default_branch` | string | no | detected after clone | Branch eligible for automatic fast-forward updates. |
| `tags` | array of strings | no | `[]` | Tags used by `--tag` filters. Manifest-source tags are combined with these tags. |

Directly declared repositories keep their configured IDs, live under
`<root>/repos/<id>`, and belong to the reserved `workspace` source.

### Credentials

`token_env` is recommended for shared configuration:

```toml
token_env = "GITHUB_TOKEN"
```

An inline `token` is stored as plaintext in `.multi-repo.toml`. If you use one,
keep the file local and restrict its permissions:

```console
chmod 600 .multi-repo.toml
```

Tokens are redacted from debug output and are not written to the state
database. Provider tokens authenticate REST discovery only; Git handles clone
and fetch authentication.

## Command reference

All commands accept `--config PATH`. Run `multi-repo <command> --help` for the
built-in reference.

### `sync`

Discovers configured repositories, updates the inventory, clones missing
repositories, fetches existing ones, and performs safe fast-forwards.

| Option | Default | Description |
| --- | --- | --- |
| `--dry-run` | off | Discover and show inventory changes without modifying state or working trees. |
| `-j, --jobs <JOBS>` | `8` | Maximum concurrent Git operations. Values below one behave as one. |
| `--color <WHEN>` | `auto` | Color dry-run changes: `auto`, `always`, or `never`. |

### `list`

Lists active repositories in the inventory.

| Option | Default | Description |
| --- | --- | --- |
| `--repo <GLOB>` | all | Select repository IDs by glob. Repeat to match any supplied glob. |
| `--source <NAME>` | all | Select repositories associated with a source. Repeat to match any supplied source. |
| `--tag <TAG>` | all | Select repositories with a tag. Repeat to match any supplied tag. |
| `--all` | off | Include inactive repositories. |
| `--json` | off | Emit a JSON array instead of text. |

Different filter categories are combined with AND; repeated values within one
category are combined with OR.

### `prune`

Removes inactive repositories only when their working trees and local Git
state are safe to delete.

| Option | Default | Description |
| --- | --- | --- |
| `--dry-run` | off | Report what would be removed without changing files or state. |

### `grep`

Searches active, successfully synchronized repository working trees. The
default output is `repository:path:line:column:text`.

```text
multi-repo grep [OPTIONS] <PATTERN> [-- <PATH>...]
```

| Option | Default | Description |
| --- | --- | --- |
| `--repo <GLOB>` | all | Select repository IDs by glob; repeat for OR matching. |
| `--source <NAME>` | all | Select source names; repeat for OR matching. |
| `--tag <TAG>` | all | Select tags; repeat for OR matching. |
| `-F, --fixed-strings` | off | Treat the pattern as a literal string instead of a regular expression. |
| `-i, --ignore-case` | off | Match case-insensitively. Conflicts with `--smart-case`. |
| `-S, --smart-case` | off | Ignore case unless the pattern contains an uppercase literal. Conflicts with `--ignore-case`. |
| `-w, --word` | off | Require word boundaries around matches. |
| `-B, --before-context <NUM>` | `0` | Print `NUM` lines before each match. |
| `-A, --after-context <NUM>` | `0` | Print `NUM` lines after each match. |
| `-C, --context <NUM>` | — | Print `NUM` lines before and after each match; overrides `-A` and `-B`. |
| `-g, --glob <GLOB>` | all | Include matching paths; prefix a glob with `!` to exclude paths. Repeatable. |
| `-l, --files-with-matches` | off | Print only files containing matches. |
| `--repos-with-matches` | off | Print only repositories containing matches. |
| `-c, --count` | off | Print matching-line counts per file. |
| `--json` | off | Emit JSON Lines. |
| `--sort-path` | off | Buffer and sort output by repository and path. |
| `--color <WHEN>` | `auto` | Color repository and path prefixes and highlight matches: `auto`, `always`, or `never`. |
| `-j, --threads <THREADS>` | `0` | Search worker threads; zero selects the count automatically. |
| `-- <PATH>...` | all | Search only these paths relative to every selected repository. |

`--files-with-matches`, `--repos-with-matches`, and `--count` are mutually
exclusive. Search includes dotfiles, respects Git and ripgrep ignore files,
skips binary files, does not follow symlinks, and always excludes `.git`.

The exit status is `0` when matches are found, `1` when no matches are found,
and `2` on error.

## Workspace data

Repositories are stored under `<root>/repos/`. The transactional inventory,
workspace lock, and temporary clones are stored under `<root>/.multi-repo/`.
Incomplete clones are not moved into `repos/`. Only one mutating operation can
run at a time, but search remains available during synchronization.

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md) for contributor setup and validation
commands.

## License

[MIT](LICENSE)
