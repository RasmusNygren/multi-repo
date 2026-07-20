use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use tokio::task::JoinSet;

use crate::config::{Config, SourceConfig};
use crate::error::{Error, Result};
use crate::git::{GitSyncResult, SyncAction, sync_repo};
use crate::model::{RepoRecord, RepoSpec, RepoStatus};
use crate::provider;
use crate::state::State;

#[derive(Clone, Copy, Debug)]
pub struct SyncOptions {
    pub jobs: usize,
    pub dry_run: bool,
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub sources: Vec<SourceReport>,
    pub repos: Vec<RepoSyncReport>,
}

#[derive(Debug)]
pub struct SourceReport {
    pub source: String,
    pub discovered: usize,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct RepoSyncReport {
    pub id: String,
    pub action: Option<SyncAction>,
    pub detail: Option<String>,
    pub error: Option<String>,
}

impl SyncReport {
    #[must_use]
    pub fn failed(&self) -> bool {
        self.sources.iter().any(|source| source.error.is_some())
            || self.repos.iter().any(|repo| repo.error.is_some())
    }
}

pub async fn synchronize(
    config: &Config,
    state: &State,
    options: SyncOptions,
) -> Result<SyncReport> {
    let _lock = if options.dry_run {
        None
    } else {
        Some(acquire_lock(&config.state_dir().join("sync.lock"))?)
    };

    let mut discovery_tasks = JoinSet::new();
    for source_config in &config.sources {
        discovery_tasks.spawn(discover_source(source_config.clone()));
    }
    let mut discoveries = Vec::with_capacity(config.sources.len());
    while let Some(result) = discovery_tasks.join_next().await {
        discoveries.push(result.map_err(|error| Error::Task(error.to_string()))?);
    }

    let mut report = SyncReport::default();
    let mut desired = BTreeMap::<String, RepoRecord>::new();
    for (source, result) in discoveries {
        match result {
            Ok(specs) => {
                report.sources.push(SourceReport {
                    source: source.clone(),
                    discovered: specs.len(),
                    error: None,
                });
                if !options.dry_run {
                    for repo in state.reconcile_source(&source, &specs, &config.repos_dir())? {
                        desired.insert(repo.id.clone(), repo);
                    }
                }
            }
            Err(error) => report.sources.push(SourceReport {
                source,
                discovered: 0,
                error: Some(error.to_string()),
            }),
        }
    }
    report
        .sources
        .sort_by(|left, right| left.source.cmp(&right.source));
    if options.dry_run {
        return Ok(report);
    }

    let state = state.clone();
    let temporary_root = config.state_dir().join("tmp");
    let mut pending = desired.into_values();
    let mut sync_tasks = JoinSet::new();
    for repo in pending.by_ref().take(options.jobs.max(1)) {
        spawn_sync_task(&mut sync_tasks, repo, state.clone(), temporary_root.clone());
    }
    while let Some(result) = sync_tasks.join_next().await {
        report
            .repos
            .push(result.map_err(|error| Error::Task(error.to_string()))?);
        if let Some(repo) = pending.next() {
            spawn_sync_task(&mut sync_tasks, repo, state.clone(), temporary_root.clone());
        }
    }
    report.repos.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(report)
}

async fn discover_source(source_config: SourceConfig) -> (String, Result<Vec<RepoSpec>>) {
    let source_name = source_config.name().to_owned();
    let result = match provider::from_config(&source_config) {
        Ok(source) => source.discover().await,
        Err(error) => Err(error),
    };
    (source_name, result)
}

fn spawn_sync_task(
    tasks: &mut JoinSet<RepoSyncReport>,
    repo: RepoRecord,
    state: State,
    temporary_root: PathBuf,
) {
    tasks.spawn(synchronize_repo(repo, state, temporary_root));
}

async fn synchronize_repo(
    repo: RepoRecord,
    state: State,
    temporary_root: PathBuf,
) -> RepoSyncReport {
    let id = repo.id.clone();
    let preserve_ready = repo.status == RepoStatus::Ready;
    let result = tokio::task::spawn_blocking(move || sync_repo(&repo, &temporary_root))
        .await
        .map_err(|error| Error::Task(error.to_string()))
        .and_then(|result| result);
    match result {
        Ok(GitSyncResult {
            action,
            detail,
            detected_default_branch,
        }) => {
            let state_result = detected_default_branch
                .as_deref()
                .map_or(Ok(()), |branch| state.record_default_branch(&id, branch))
                .and_then(|()| state.mark_ready(&id));
            match state_result {
                Ok(()) => RepoSyncReport {
                    id,
                    action: Some(action),
                    detail,
                    error: None,
                },
                Err(error) => RepoSyncReport {
                    id,
                    action: Some(action),
                    detail,
                    error: Some(error.to_string()),
                },
            }
        }
        Err(error) => {
            let message = error.to_string();
            let state_error = state
                .mark_error(&id, &message, preserve_ready)
                .err()
                .map(|error| format!("; failed to record error: {error}"))
                .unwrap_or_default();
            RepoSyncReport {
                id,
                action: None,
                detail: None,
                error: Some(format!("{message}{state_error}")),
            }
        }
    }
}

fn acquire_lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|source| Error::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.try_lock_exclusive().map_err(|_| Error::SyncLocked)?;
    Ok(file)
}
