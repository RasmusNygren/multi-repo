use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use tokio::task::JoinSet;

use crate::config::{Config, SourceConfig, WORKSPACE_SOURCE_NAME, validate_repo_name};
use crate::error::{Error, Result};
use crate::git::{GitSyncResult, SyncAction, sync_repo};
use crate::language;
use crate::lock::acquire_workspace_lock;
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
    pub preview: Option<SyncPreview>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncProgress {
    Discovering { completed: usize, total: usize },
    Repositories { completed: usize, total: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryChangeKind {
    Activate,
    Deactivate,
}

#[derive(Debug, Eq, PartialEq)]
pub struct RepositoryChange {
    pub id: String,
    pub kind: RepositoryChangeKind,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct SyncPreview {
    pub changes: Vec<RepositoryChange>,
    pub unchanged: usize,
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
    pub warning: Option<String>,
    pub error: Option<String>,
}

impl SyncReport {
    #[must_use]
    pub fn failed(&self) -> bool {
        self.sources.iter().any(|source| source.error.is_some())
            || self.repos.iter().any(|repo| repo.error.is_some())
    }
}

/// Discovers configured repositories and synchronizes their working trees.
///
/// A dry run performs discovery only and does not create local state.
///
/// # Errors
///
/// Returns an error if synchronization is already running or local state,
/// task scheduling, or inventory reconciliation fails. Individual provider
/// and Git failures are captured in the returned report.
pub async fn synchronize(config: &Config, options: SyncOptions) -> Result<SyncReport> {
    synchronize_with_progress(config, options, |_| {}).await
}

/// Synchronizes repositories and reports discovery and Git-operation progress.
///
/// # Errors
///
/// Returns the same errors as [`synchronize`].
pub async fn synchronize_with_progress(
    config: &Config,
    options: SyncOptions,
    mut progress: impl FnMut(SyncProgress),
) -> Result<SyncReport> {
    let _lock = if options.dry_run {
        None
    } else {
        Some(acquire_workspace_lock(
            &config.state_dir().join("sync.lock"),
        )?)
    };

    let discovery_total = config.sources.len() + usize::from(!config.repositories.is_empty());
    let mut discovery_completed = 0;
    progress(SyncProgress::Discovering {
        completed: discovery_completed,
        total: discovery_total,
    });
    let mut discovery_tasks = JoinSet::new();
    for source_config in &config.sources {
        discovery_tasks.spawn(discover_source(source_config.clone()));
    }
    let mut discoveries =
        Vec::with_capacity(config.sources.len() + usize::from(!config.repositories.is_empty()));
    if !config.repositories.is_empty() {
        discoveries.push((
            WORKSPACE_SOURCE_NAME.to_owned(),
            provider::discover_repositories(&config.repositories),
        ));
        discovery_completed += 1;
        progress(SyncProgress::Discovering {
            completed: discovery_completed,
            total: discovery_total,
        });
    }
    while let Some(result) = discovery_tasks.join_next().await {
        discoveries.push(result.map_err(|error| Error::Task(error.to_string()))?);
        discovery_completed += 1;
        progress(SyncProgress::Discovering {
            completed: discovery_completed,
            total: discovery_total,
        });
    }

    let mut report = SyncReport::default();
    let mut successful = Vec::new();
    for (source, result) in discoveries {
        match result {
            Ok(specs) => {
                report.sources.push(SourceReport {
                    source: source.clone(),
                    discovered: specs.len(),
                    error: None,
                });
                successful.push((source, specs));
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
    if config.repositories.is_empty() {
        successful.push((WORKSPACE_SOURCE_NAME.to_owned(), Vec::new()));
    }
    if options.dry_run {
        let current = State::list_existing(&config.state_dir())?;
        report.preview = Some(preview_reconciliation(&current, &successful)?);
        return Ok(report);
    }

    let state = State::initialize(&config.state_dir())?;
    let desired = reconcile_repositories(&state, config, successful)?;
    let total = desired.len();
    let mut completed = 0;
    progress(SyncProgress::Repositories { completed, total });
    let mut pending = desired.into_iter();
    let mut sync_tasks = JoinSet::new();
    for repo in pending.by_ref().take(options.jobs.max(1)) {
        spawn_sync_task(&mut sync_tasks, repo, state.clone(), config);
    }
    while let Some(result) = sync_tasks.join_next().await {
        report
            .repos
            .push(result.map_err(|error| Error::Task(error.to_string()))?);
        completed += 1;
        progress(SyncProgress::Repositories { completed, total });
        if let Some(repo) = pending.next() {
            spawn_sync_task(&mut sync_tasks, repo, state.clone(), config);
        }
    }
    report.repos.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(report)
}

fn reconcile_repositories(
    state: &State,
    config: &Config,
    successful: Vec<(String, Vec<RepoSpec>)>,
) -> Result<Vec<RepoRecord>> {
    let mut desired_ids = BTreeSet::new();
    for (source, specs) in successful {
        desired_ids.extend(state.reconcile_source(&source, &specs, &config.repos_dir())?);
    }
    Ok(state
        .list(false)?
        .into_iter()
        .filter(|repo| desired_ids.contains(&repo.id))
        .collect())
}

fn preview_reconciliation(
    current: &[RepoRecord],
    discoveries: &[(String, Vec<RepoSpec>)],
) -> Result<SyncPreview> {
    let current_active = current
        .iter()
        .filter(|repo| repo.active)
        .map(|repo| repo.id.clone())
        .collect::<BTreeSet<_>>();
    let mut associations = current
        .iter()
        .map(|repo| (repo.id.clone(), repo.sources.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut ids_by_url = current
        .iter()
        .map(|repo| (repo.canonical_url.clone(), repo.id.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut urls_by_id = current
        .iter()
        .map(|repo| (repo.id.clone(), repo.canonical_url.clone()))
        .collect::<BTreeMap<_, _>>();

    for (source, specs) in discoveries {
        for sources in associations.values_mut() {
            sources.remove(source);
        }
        for spec in specs {
            validate_repo_name(&spec.id)?;
            let id = if let Some(id) = ids_by_url.get(&spec.canonical_url) {
                id.clone()
            } else {
                if let Some(existing_url) = urls_by_id.get(&spec.id) {
                    return Err(Error::Config(format!(
                        "repository id {:?} refers to both {existing_url:?} and {:?}",
                        spec.id, spec.canonical_url
                    )));
                }
                ids_by_url.insert(spec.canonical_url.clone(), spec.id.clone());
                urls_by_id.insert(spec.id.clone(), spec.canonical_url.clone());
                spec.id.clone()
            };
            associations.entry(id).or_default().insert(source.clone());
        }
    }

    let desired_active = associations
        .into_iter()
        .filter_map(|(id, sources)| (!sources.is_empty()).then_some(id))
        .collect::<BTreeSet<_>>();
    let mut changes = desired_active
        .difference(&current_active)
        .map(|id| RepositoryChange {
            id: id.clone(),
            kind: RepositoryChangeKind::Activate,
        })
        .chain(
            current_active
                .difference(&desired_active)
                .map(|id| RepositoryChange {
                    id: id.clone(),
                    kind: RepositoryChangeKind::Deactivate,
                }),
        )
        .collect::<Vec<_>>();
    changes.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(SyncPreview {
        changes,
        unchanged: current_active.intersection(&desired_active).count(),
    })
}

async fn discover_source(source_config: SourceConfig) -> (String, Result<Vec<RepoSpec>>) {
    let source_name = source_config.name().to_owned();
    let result = provider::discover(&source_config).await;
    (source_name, result)
}

fn spawn_sync_task(
    tasks: &mut JoinSet<RepoSyncReport>,
    repo: RepoRecord,
    state: State,
    config: &Config,
) {
    let fetch_all_branches = fetch_all_branches(config, &repo);
    tasks.spawn(synchronize_repo(
        repo,
        state,
        config.state_dir().join("tmp"),
        fetch_all_branches,
    ));
}

fn fetch_all_branches(config: &Config, repo: &RepoRecord) -> bool {
    config
        .sources
        .iter()
        .any(|source| source.fetch_all_branches() && repo.sources.contains(source.name()))
}

async fn synchronize_repo(
    repo: RepoRecord,
    state: State,
    temporary_root: PathBuf,
    fetch_all_branches: bool,
) -> RepoSyncReport {
    let id = repo.id.clone();
    let preserve_ready = repo.status == RepoStatus::Ready;
    let result = tokio::task::spawn_blocking(move || {
        let git_result = sync_repo(&repo, &temporary_root, fetch_all_branches)?;
        let detected_tags = git_result
            .detected_default_branch
            .as_deref()
            .map(|branch| language::detect(&repo.local_path, branch));
        Ok::<_, Error>((git_result, detected_tags))
    })
    .await
    .map_err(|error| Error::Task(error.to_string()))
    .and_then(|result| result);
    match result {
        Ok((
            GitSyncResult {
                action,
                detail,
                detected_default_branch,
            },
            detected_tags,
        )) => {
            let (detected_tags, warning) = match detected_tags {
                Some(Ok(tags)) => (Some(tags), None),
                Some(Err(error)) => (None, Some(format!("language detection failed: {error}"))),
                None => (None, None),
            };
            let state_result = detected_default_branch
                .as_deref()
                .map_or(Ok(()), |branch| state.record_default_branch(&id, branch))
                .and_then(|()| {
                    detected_tags
                        .as_ref()
                        .map_or(Ok(()), |tags| state.replace_detected_tags(&id, tags))
                })
                .and_then(|()| state.mark_ready(&id));
            match state_result {
                Ok(()) => RepoSyncReport {
                    id,
                    action: Some(action),
                    detail,
                    warning,
                    error: None,
                },
                Err(error) => RepoSyncReport {
                    id,
                    action: Some(action),
                    detail,
                    warning,
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
                warning: None,
                error: Some(format!("{message}{state_error}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ManifestConfig;

    fn record(id: &str, url: &str, active: bool, sources: &[&str]) -> RepoRecord {
        RepoRecord {
            id: id.into(),
            canonical_url: url.into(),
            clone_url: url.into(),
            local_path: PathBuf::from("repos").join(id),
            default_branch: Some("main".into()),
            active,
            status: RepoStatus::Ready,
            sources: sources.iter().map(|source| (*source).to_owned()).collect(),
            tags: BTreeSet::new(),
            last_error: None,
        }
    }

    fn spec(id: &str, url: &str) -> RepoSpec {
        RepoSpec {
            id: id.into(),
            canonical_url: url.into(),
            clone_url: url.into(),
            default_branch: Some("main".into()),
            tags: BTreeSet::new(),
        }
    }

    #[test]
    fn preview_reports_activation_deactivation_and_shared_repositories() {
        let current = vec![
            record("github/dormant", "host/dormant", false, &[]),
            record("github/old", "host/old", true, &["github"]),
            record(
                "github/shared",
                "host/shared",
                true,
                &["github", "manifest"],
            ),
        ];
        let discoveries = vec![(
            "github".to_owned(),
            vec![
                spec("github/new", "host/new"),
                spec("github/renamed-dormant", "host/dormant"),
            ],
        )];

        let preview = preview_reconciliation(&current, &discoveries).unwrap();

        assert_eq!(
            preview,
            SyncPreview {
                changes: vec![
                    RepositoryChange {
                        id: "github/dormant".into(),
                        kind: RepositoryChangeKind::Activate,
                    },
                    RepositoryChange {
                        id: "github/new".into(),
                        kind: RepositoryChangeKind::Activate,
                    },
                    RepositoryChange {
                        id: "github/old".into(),
                        kind: RepositoryChangeKind::Deactivate,
                    },
                ],
                unchanged: 1,
            }
        );
    }

    #[test]
    fn fetches_all_branches_when_any_associated_source_enables_it() {
        let config = Config {
            root: PathBuf::from("workspace"),
            sources: vec![
                SourceConfig::Manifest(ManifestConfig {
                    name: "default-only".into(),
                    fetch_all_branches: false,
                    path: PathBuf::from("default.toml"),
                    tags: Vec::new(),
                }),
                SourceConfig::Manifest(ManifestConfig {
                    name: "all-branches".into(),
                    fetch_all_branches: true,
                    path: PathBuf::from("all.toml"),
                    tags: Vec::new(),
                }),
            ],
            repositories: Vec::new(),
        };

        assert!(!fetch_all_branches(
            &config,
            &record("default", "host/default", true, &["default-only"]),
        ));
        assert!(fetch_all_branches(
            &config,
            &record(
                "shared",
                "host/shared",
                true,
                &["default-only", "all-branches"],
            ),
        ));
        assert!(!fetch_all_branches(
            &config,
            &record("inline", "host/inline", true, &[WORKSPACE_SOURCE_NAME]),
        ));
    }
}
