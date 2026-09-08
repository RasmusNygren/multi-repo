use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::Builder;

use crate::error::{Error, Result};
use crate::model::RepoRecord;
use crate::provider::canonicalize_remote;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncAction {
    Cloned,
    FastForwarded,
    Fetched,
    Unchanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitSyncResult {
    pub action: SyncAction,
    pub detail: Option<String>,
    pub detected_default_branch: Option<String>,
}

pub(crate) fn sync_repo(
    repo: &RepoRecord,
    temporary_root: &Path,
    fetch_all_branches: bool,
) -> Result<GitSyncResult> {
    if !repo.local_path.exists() {
        return clone_repo(repo, temporary_root, fetch_all_branches);
    }
    if !repo.local_path.join(".git").exists() {
        return Err(Error::Git(format!(
            "{} exists but is not a Git working tree",
            repo.local_path.display()
        )));
    }

    // `git remote get-url` expands `url.*.insteadOf` rules. Compare the URL
    // stored in the repository instead, so transport rewrites (including SSH
    // host aliases) do not make the same repository appear to be a different
    // remote.
    let origin = git_stdout(&repo.local_path, ["config", "--get", "remote.origin.url"])?;
    if canonicalize_remote(&origin)? != repo.canonical_url {
        return Err(Error::Git(format!(
            "{} has origin {:?}, expected {:?}",
            repo.local_path.display(),
            origin,
            repo.clone_url
        )));
    }

    if fetch_all_branches {
        // An explicit wildcard also fetches every branch in older single-branch clones.
        git_success(
            &repo.local_path,
            [
                "fetch",
                "--quiet",
                "--prune",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        )?;
    } else if let Some(default_branch) = &repo.default_branch {
        let refspec = format!("+refs/heads/{default_branch}:refs/remotes/origin/{default_branch}");
        git_success(
            &repo.local_path,
            ["fetch", "--quiet", "--prune", "origin", &refspec],
        )?;
    } else {
        git_success(&repo.local_path, ["fetch", "--quiet", "--prune", "origin"])?;
    }

    let detected_default_branch = default_branch(repo)?;
    let dirty = !working_tree_is_clean(&repo.local_path)?;
    let branch = git_optional_stdout(
        &repo.local_path,
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?;
    let Some(default_branch) = detected_default_branch.as_deref() else {
        return Ok(result(
            SyncAction::Fetched,
            Some("default branch is unknown; fetched without updating the worktree".into()),
            None,
        ));
    };
    let Some(branch) = branch else {
        return Ok(result(
            SyncAction::Fetched,
            Some("detached HEAD; fetched without updating the worktree".into()),
            Some(default_branch),
        ));
    };
    if branch != default_branch {
        return Ok(result(
            SyncAction::Fetched,
            Some(format!(
                "on branch {branch:?}; fetched without switching to {default_branch:?}"
            )),
            Some(default_branch),
        ));
    }
    if dirty {
        return Ok(result(
            SyncAction::Fetched,
            Some("working tree has local changes; fetched without updating it".into()),
            Some(default_branch),
        ));
    }

    let upstream = format!("origin/{default_branch}");
    let head = git_stdout(&repo.local_path, ["rev-parse", "HEAD"])?;
    let remote_head = git_stdout(&repo.local_path, ["rev-parse", upstream.as_str()])?;
    if head == remote_head {
        return Ok(result(SyncAction::Unchanged, None, Some(default_branch)));
    }
    let ancestor = Command::new("git")
        .arg("-C")
        .arg(&repo.local_path)
        .args(["merge-base", "--is-ancestor", "HEAD", upstream.as_str()])
        .output()
        .map_err(|error| git_spawn_error(&repo.local_path, &error))?;
    if ancestor.status.success() {
        git_success(
            &repo.local_path,
            ["merge", "--quiet", "--ff-only", upstream.as_str()],
        )?;
        Ok(result(
            SyncAction::FastForwarded,
            None,
            Some(default_branch),
        ))
    } else if ancestor.status.code() == Some(1) {
        Ok(result(
            SyncAction::Fetched,
            Some("local default branch has diverged; fetched without updating it".into()),
            Some(default_branch),
        ))
    } else {
        Err(command_error(&repo.local_path, &ancestor))
    }
}

pub(crate) fn working_tree_is_clean(path: &Path) -> Result<bool> {
    if !path.join(".git").exists() {
        return Err(Error::Git(format!(
            "{} is not a Git working tree",
            path.display()
        )));
    }
    Ok(git_stdout(
        path,
        ["status", "--porcelain=v1", "--untracked-files=normal"],
    )?
    .is_empty())
}

pub(crate) fn has_local_git_state(path: &Path) -> Result<bool> {
    if !git_stdout(path, ["stash", "list", "--format=%gd"])?.is_empty() {
        return Ok(true);
    }
    if git_stdout(
        path,
        ["rev-list", "--count", "--branches", "--not", "--remotes"],
    )? != "0"
    {
        return Ok(true);
    }
    let worktrees = git_stdout(path, ["worktree", "list", "--porcelain"])?
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count();
    Ok(worktrees > 1)
}

fn clone_repo(
    repo: &RepoRecord,
    temporary_root: &Path,
    fetch_all_branches: bool,
) -> Result<GitSyncResult> {
    std::fs::create_dir_all(temporary_root).map_err(|source| Error::Write {
        path: temporary_root.to_path_buf(),
        source,
    })?;
    let temporary = Builder::new()
        .prefix("clone-")
        .tempdir_in(temporary_root)
        .map_err(|source| Error::Write {
            path: temporary_root.to_path_buf(),
            source,
        })?;

    let mut command = Command::new("git");
    command.args(["clone", "--quiet"]);
    if !fetch_all_branches {
        command.arg("--single-branch");
    }
    if let Some(branch) = &repo.default_branch {
        command.args(["--branch", branch]);
    }
    command.arg(&repo.clone_url).arg(temporary.path());
    let output = command
        .output()
        .map_err(|error| git_spawn_error(temporary.path(), &error))?;
    if !output.status.success() {
        return Err(command_error(temporary.path(), &output));
    }
    if let Some(parent) = repo.local_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::rename(temporary.path(), &repo.local_path).map_err(|source| Error::Write {
        path: repo.local_path.clone(),
        source,
    })?;
    let _ = temporary.keep();
    let detected_default_branch = default_branch(repo)?;
    Ok(GitSyncResult {
        action: SyncAction::Cloned,
        detail: None,
        detected_default_branch,
    })
}

fn remote_default_branch(path: &Path) -> Result<Option<String>> {
    Ok(git_optional_stdout(
        path,
        [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )?
    .and_then(|reference| reference.strip_prefix("origin/").map(str::to_owned)))
}

fn default_branch(repo: &RepoRecord) -> Result<Option<String>> {
    repo.default_branch.clone().map_or_else(
        || remote_default_branch(&repo.local_path),
        |branch| Ok(Some(branch)),
    )
}

fn result(
    action: SyncAction,
    detail: Option<String>,
    default_branch: Option<&str>,
) -> GitSyncResult {
    GitSyncResult {
        action,
        detail,
        detected_default_branch: default_branch.map(str::to_owned),
    }
}

fn git_stdout<I, S>(path: &Path, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| git_spawn_error(path, &error))?;
    if !output.status.success() {
        return Err(command_error(path, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn git_optional_stdout<I, S>(path: &Path, args: I) -> Result<Option<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| git_spawn_error(path, &error))?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(command_error(path, &output))
    }
}

fn git_success<I, S>(path: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_stdout(path, args).map(drop)
}

fn git_spawn_error(path: &Path, error: &std::io::Error) -> Error {
    Error::Git(format!("could not run git in {}: {error}", path.display()))
}

fn command_error(path: &Path, output: &Output) -> Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    Error::Git(format!(
        "git in {} exited with {}: {}",
        path.display(),
        output.status,
        stderr.trim()
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::model::RepoStatus;

    #[test]
    fn clones_fast_forwards_and_preserves_dirty_worktrees() {
        let temp = TempDir::new().unwrap();
        let remote = temp.path().join("remote.git");
        let seed = temp.path().join("seed");
        run(
            temp.path(),
            [
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        run(
            temp.path(),
            ["init", "--initial-branch=main", seed.to_str().unwrap()],
        );
        run(&seed, ["config", "user.name", "Test"]);
        run(&seed, ["config", "user.email", "test@example.com"]);
        fs::write(seed.join("file.txt"), "one\n").unwrap();
        run(&seed, ["add", "file.txt"]);
        run(&seed, ["commit", "-m", "initial"]);
        run(&seed, ["remote", "add", "origin", remote.to_str().unwrap()]);
        run(&seed, ["push", "-u", "origin", "main"]);

        let checkout = temp.path().join("managed/repo");
        let repo = RepoRecord {
            id: "manual/repo".into(),
            canonical_url: canonicalize_remote(remote.to_str().unwrap()).unwrap(),
            clone_url: remote.to_string_lossy().into_owned(),
            local_path: checkout.clone(),
            default_branch: None,
            active: true,
            status: RepoStatus::Pending,
            sources: BTreeSet::from(["manual".into()]),
            tags: BTreeSet::new(),
            last_error: None,
        };
        let cloned = sync_repo(&repo, &temp.path().join("tmp"), false).unwrap();
        assert_eq!(cloned.action, SyncAction::Cloned);
        assert_eq!(cloned.detected_default_branch.as_deref(), Some("main"));

        fs::write(seed.join("file.txt"), "two\n").unwrap();
        run(&seed, ["add", "file.txt"]);
        run(&seed, ["commit", "-m", "update"]);
        run(&seed, ["push", "origin", "main"]);
        fs::write(checkout.join("local.txt"), "do not discard\n").unwrap();
        let fetched = sync_repo(&repo, &temp.path().join("tmp"), false).unwrap();
        assert_eq!(fetched.action, SyncAction::Fetched);
        assert!(fetched.detail.unwrap().contains("local changes"));
        assert_eq!(
            fs::read_to_string(checkout.join("file.txt")).unwrap(),
            "one\n"
        );
        assert_eq!(
            fs::read_to_string(checkout.join("local.txt")).unwrap(),
            "do not discard\n"
        );

        fs::remove_file(checkout.join("local.txt")).unwrap();
        assert_eq!(
            sync_repo(&repo, &temp.path().join("tmp"), false)
                .unwrap()
                .action,
            SyncAction::FastForwarded
        );
        assert_eq!(
            fs::read_to_string(checkout.join("file.txt")).unwrap(),
            "two\n"
        );

        run(&checkout, ["config", "user.name", "Test"]);
        run(&checkout, ["config", "user.email", "test@example.com"]);
        fs::write(checkout.join("local-commit.txt"), "local history\n").unwrap();
        run(&checkout, ["add", "local-commit.txt"]);
        run(&checkout, ["commit", "-m", "local commit"]);
        fs::write(seed.join("remote-commit.txt"), "remote history\n").unwrap();
        run(&seed, ["add", "remote-commit.txt"]);
        run(&seed, ["commit", "-m", "remote commit"]);
        run(&seed, ["push", "origin", "main"]);

        let diverged = sync_repo(&repo, &temp.path().join("tmp"), false).unwrap();
        assert_eq!(diverged.action, SyncAction::Fetched);
        assert!(diverged.detail.unwrap().contains("diverged"));
        assert!(checkout.join("local-commit.txt").exists());
        assert!(!checkout.join("remote-commit.txt").exists());
    }

    #[test]
    fn accepts_origin_rewritten_by_instead_of() {
        let temp = TempDir::new().unwrap();
        let remote = temp.path().join("repo.git");
        let seed = temp.path().join("seed");
        run(
            temp.path(),
            [
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        run(
            temp.path(),
            ["init", "--initial-branch=main", seed.to_str().unwrap()],
        );
        run(&seed, ["config", "user.name", "Test"]);
        run(&seed, ["config", "user.email", "test@example.com"]);
        fs::write(seed.join("file.txt"), "content\n").unwrap();
        run(&seed, ["add", "file.txt"]);
        run(&seed, ["commit", "-m", "initial"]);
        run(&seed, ["remote", "add", "origin", remote.to_str().unwrap()]);
        run(&seed, ["push", "-u", "origin", "main"]);

        let checkout = temp.path().join("checkout");
        run(
            temp.path(),
            [
                "clone",
                remote.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );

        let clone_url = "https://git.example/acme/repo.git";
        run(&checkout, ["remote", "set-url", "origin", clone_url]);
        let rewrite_base = format!("file://{}/", temp.path().display());
        let rewrite_key = format!("url.{rewrite_base}.insteadOf");
        run(
            &checkout,
            ["config", rewrite_key.as_str(), "https://git.example/acme/"],
        );

        assert_eq!(
            git_stdout(&checkout, ["config", "--get", "remote.origin.url"]).unwrap(),
            clone_url
        );
        assert_eq!(
            git_stdout(&checkout, ["remote", "get-url", "origin"]).unwrap(),
            format!("{rewrite_base}repo.git")
        );

        let repo = RepoRecord {
            id: "manual/repo".into(),
            canonical_url: canonicalize_remote(clone_url).unwrap(),
            clone_url: clone_url.into(),
            local_path: checkout,
            default_branch: Some("main".into()),
            active: true,
            status: RepoStatus::Pending,
            sources: BTreeSet::from(["manual".into()]),
            tags: BTreeSet::new(),
            last_error: None,
        };

        assert_eq!(
            sync_repo(&repo, &temp.path().join("tmp"), false)
                .unwrap()
                .action,
            SyncAction::Unchanged
        );
    }

    fn run<I, S>(path: &Path, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
