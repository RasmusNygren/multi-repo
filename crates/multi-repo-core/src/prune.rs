use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::{has_local_git_state, working_tree_is_clean};
use crate::lock::acquire_workspace_lock;
use crate::model::RepoRecord;
use crate::state::State;

#[derive(Clone, Copy, Debug, Default)]
pub struct PruneOptions {
    pub dry_run: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PruneAction {
    Deleted,
    WouldDelete,
    RemovedMissing,
    WouldRemoveMissing,
    SkippedDirty,
    SkippedLocalState,
}

#[derive(Debug)]
pub struct RepoPruneReport {
    pub id: String,
    pub path: PathBuf,
    pub action: Option<PruneAction>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct PruneReport {
    pub repos: Vec<RepoPruneReport>,
}

impl PruneReport {
    #[must_use]
    pub fn failed(&self) -> bool {
        self.repos.iter().any(|repo| repo.error.is_some())
    }
}

/// Deletes inactive repositories whose Git working trees are clean.
///
/// Missing working trees have their stale inventory records removed. Dirty,
/// untracked, invalid, and unverifiable working trees are retained.
///
/// # Errors
///
/// Returns an error if another mutating operation holds the workspace lock or
/// the state inventory cannot be opened or listed. Per-repository failures are
/// captured in the returned report.
pub fn prune(config: &Config, options: PruneOptions) -> Result<PruneReport> {
    let _lock = acquire_workspace_lock(&config.state_dir().join("sync.lock"))?;
    let state = State::initialize(&config.state_dir())?;
    let repos_dir = config.repos_dir();
    let mut report = PruneReport::default();
    for repo in state.list(true)?.into_iter().filter(|repo| !repo.active) {
        report
            .repos
            .push(prune_repo(&state, &repos_dir, &repo, options));
    }
    if !options.dry_run {
        let retained_repositories = state
            .list(true)?
            .into_iter()
            .map(|repo| repo.local_path)
            .collect();
        remove_empty_managed_directories(&repos_dir, &retained_repositories)?;
    }
    Ok(report)
}

fn prune_repo(
    state: &State,
    repos_dir: &std::path::Path,
    repo: &RepoRecord,
    options: PruneOptions,
) -> RepoPruneReport {
    let result = prune_working_tree(state, repos_dir, repo, options);
    RepoPruneReport {
        id: repo.id.clone(),
        path: repo.local_path.clone(),
        action: result.as_ref().ok().copied(),
        error: result.err().map(|error| error.to_string()),
    }
}

fn prune_working_tree(
    state: &State,
    repos_dir: &std::path::Path,
    repo: &RepoRecord,
    options: PruneOptions,
) -> Result<PruneAction> {
    if repos_dir.is_symlink() || !repo.local_path.starts_with(repos_dir) {
        return Err(Error::Config(format!(
            "refusing to prune {} outside managed repository directory {}",
            repo.local_path.display(),
            repos_dir.display()
        )));
    }
    let metadata = match std::fs::symlink_metadata(&repo.local_path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            if options.dry_run {
                return Ok(PruneAction::WouldRemoveMissing);
            }
            state.remove_inactive(&repo.id)?;
            return Ok(PruneAction::RemovedMissing);
        }
        Err(source) => {
            return Err(Error::Read {
                path: repo.local_path.clone(),
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(Error::Config(format!(
            "refusing to prune {} because it is not a directory",
            repo.local_path.display()
        )));
    }
    let root = repos_dir.canonicalize().map_err(|source| Error::Read {
        path: repos_dir.to_path_buf(),
        source,
    })?;
    let path = repo
        .local_path
        .canonicalize()
        .map_err(|source| Error::Read {
            path: repo.local_path.clone(),
            source,
        })?;
    if path == root || !path.starts_with(&root) {
        return Err(Error::Config(format!(
            "refusing to prune {} because it resolves outside the managed repository directory or to its root",
            repo.local_path.display()
        )));
    }
    if !working_tree_is_clean(&path)? {
        return Ok(PruneAction::SkippedDirty);
    }
    if has_local_git_state(&path)? {
        return Ok(PruneAction::SkippedLocalState);
    }
    if options.dry_run {
        return Ok(PruneAction::WouldDelete);
    }
    std::fs::remove_dir_all(&path).map_err(|source| Error::Write {
        path: repo.local_path.clone(),
        source,
    })?;
    state.remove_inactive(&repo.id)?;
    Ok(PruneAction::Deleted)
}

fn remove_empty_managed_directories(
    directory: &Path,
    retained_repositories: &HashSet<PathBuf>,
) -> Result<()> {
    if directory.is_symlink()
        || retained_repositories.contains(directory)
        || directory.join(".git").exists()
    {
        return Ok(());
    }
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Error::Read {
                path: directory.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| Error::Read {
            path: directory.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| Error::Read {
            path: entry.path(),
            source,
        })?;
        if file_type.is_dir() {
            remove_empty_managed_directories(&entry.path(), retained_repositories)?;
        }
    }
    match std::fs::remove_dir(directory) {
        Ok(()) => Ok(()),
        Err(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            Ok(())
        }
        Err(source) => Err(Error::Write {
            path: directory.to_path_buf(),
            source,
        }),
    }
}
