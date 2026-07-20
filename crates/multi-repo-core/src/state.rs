use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::config::validate_repo_name;
use crate::error::{Error, Result};
use crate::model::{RepoRecord, RepoSpec, RepoStatus};

const SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug)]
pub struct State {
    path: PathBuf,
}

impl State {
    pub fn initialize(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir).map_err(|source| Error::Write {
            path: state_dir.to_path_buf(),
            source,
        })?;
        let state = Self {
            path: state_dir.join("state.sqlite3"),
        };
        let connection = state.connect()?;
        connection.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS repos (
                id TEXT PRIMARY KEY,
                canonical_url TEXT NOT NULL UNIQUE,
                clone_url TEXT NOT NULL,
                local_path TEXT NOT NULL UNIQUE,
                default_branch TEXT,
                active INTEGER NOT NULL DEFAULT 1,
                status TEXT NOT NULL DEFAULT 'pending',
                last_error TEXT
            );
            CREATE TABLE IF NOT EXISTS repo_sources (
                repo_id TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
                source TEXT NOT NULL,
                tags_json TEXT NOT NULL DEFAULT '[]',
                active INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (repo_id, source)
            );
            ",
        )?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version != SCHEMA_VERSION {
            return Err(Error::Config(format!(
                "unsupported state schema {version}; expected {SCHEMA_VERSION}"
            )));
        }
        Ok(state)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn reconcile_source(
        &self,
        source: &str,
        specs: &[RepoSpec],
        repos_dir: &Path,
    ) -> Result<Vec<RepoRecord>> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE repo_sources SET active = 0 WHERE source = ?1",
            [source],
        )?;

        let mut ids = Vec::with_capacity(specs.len());
        for spec in specs {
            if spec.source != source {
                return Err(Error::Config(format!(
                    "source {source:?} returned repository {:?} for source {:?}",
                    spec.id, spec.source
                )));
            }
            validate_repo_name(&spec.id)?;
            let id = repository_id(&transaction, spec)?;
            let local_path = repos_dir.join(&id);
            let local_path = path_as_text(&local_path)?;
            transaction.execute(
                "INSERT INTO repos (
                    id, canonical_url, clone_url, local_path, default_branch, active, status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1, 'pending')
                 ON CONFLICT(id) DO UPDATE SET
                    clone_url = excluded.clone_url,
                    default_branch = COALESCE(excluded.default_branch, repos.default_branch)",
                params![
                    id,
                    spec.canonical_url,
                    spec.clone_url,
                    local_path,
                    spec.default_branch
                ],
            )?;
            let tags = serde_json::to_string(&spec.tags)?;
            transaction.execute(
                "INSERT INTO repo_sources (repo_id, source, tags_json, active)
                 VALUES (?1, ?2, ?3, 1)
                 ON CONFLICT(repo_id, source) DO UPDATE SET
                    tags_json = excluded.tags_json,
                    active = 1",
                params![id, source, tags],
            )?;
            ids.push(id);
        }
        transaction.execute(
            "UPDATE repos SET active = EXISTS(
                SELECT 1 FROM repo_sources
                WHERE repo_sources.repo_id = repos.id AND repo_sources.active = 1
             )",
            [],
        )?;
        transaction.commit()?;
        ids.into_iter().map(|id| self.get(&id)).collect()
    }

    pub fn mark_ready(&self, id: &str) -> Result<()> {
        self.set_result(id, RepoStatus::Ready, None)
    }

    pub(crate) fn record_default_branch(&self, id: &str, branch: &str) -> Result<()> {
        let connection = self.connect()?;
        connection.execute(
            "UPDATE repos SET default_branch = COALESCE(default_branch, ?2) WHERE id = ?1",
            params![id, branch],
        )?;
        Ok(())
    }

    pub fn mark_error(&self, id: &str, error: &str, preserve_ready: bool) -> Result<()> {
        let connection = self.connect()?;
        if preserve_ready {
            connection.execute(
                "UPDATE repos SET last_error = ?2 WHERE id = ?1",
                params![id, error],
            )?;
        } else {
            connection.execute(
                "UPDATE repos SET status = 'error', last_error = ?2 WHERE id = ?1",
                params![id, error],
            )?;
        }
        Ok(())
    }

    pub fn list(&self, include_inactive: bool) -> Result<Vec<RepoRecord>> {
        let connection = self.connect()?;
        let sql = if include_inactive {
            "SELECT id FROM repos ORDER BY id"
        } else {
            "SELECT id FROM repos WHERE active = 1 ORDER BY id"
        };
        let mut statement = connection.prepare(sql)?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        ids.into_iter().map(|id| self.get(&id)).collect()
    }

    pub fn searchable(&self) -> Result<Vec<RepoRecord>> {
        Ok(self
            .list(false)?
            .into_iter()
            .filter(|repo| repo.status == RepoStatus::Ready && repo.local_path.is_dir())
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<RepoRecord> {
        let connection = self.connect()?;
        let mut record = connection.query_row(
            "SELECT id, canonical_url, clone_url, local_path, default_branch,
                    active, status, last_error
             FROM repos WHERE id = ?1",
            [id],
            |row| {
                let status: String = row.get(6)?;
                Ok(RepoRecord {
                    id: row.get(0)?,
                    canonical_url: row.get(1)?,
                    clone_url: row.get(2)?,
                    local_path: PathBuf::from(row.get::<_, String>(3)?),
                    default_branch: row.get(4)?,
                    active: row.get(5)?,
                    status: RepoStatus::parse(&status).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    sources: BTreeSet::new(),
                    tags: BTreeSet::new(),
                    last_error: row.get(7)?,
                })
            },
        )?;
        let mut statement = connection.prepare(
            "SELECT source, tags_json FROM repo_sources
             WHERE repo_id = ?1 AND active = 1 ORDER BY source",
        )?;
        let associations = statement
            .query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (source, tags) in associations {
            record.sources.insert(source);
            record
                .tags
                .extend(serde_json::from_str::<BTreeSet<String>>(&tags)?);
        }
        Ok(record)
    }

    fn set_result(&self, id: &str, status: RepoStatus, error: Option<&str>) -> Result<()> {
        let connection = self.connect()?;
        connection.execute(
            "UPDATE repos SET status = ?2, last_error = ?3 WHERE id = ?1",
            params![id, status.as_str(), error],
        )?;
        Ok(())
    }

    fn connect(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }
}

fn repository_id(transaction: &Transaction<'_>, spec: &RepoSpec) -> Result<String> {
    if let Some(id) = transaction
        .query_row(
            "SELECT id FROM repos WHERE canonical_url = ?1",
            [&spec.canonical_url],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    if let Some(existing_url) = transaction
        .query_row(
            "SELECT canonical_url FROM repos WHERE id = ?1",
            [&spec.id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        return Err(Error::Config(format!(
            "repository id {:?} refers to both {existing_url:?} and {:?}",
            spec.id, spec.canonical_url
        )));
    }
    Ok(spec.id.clone())
}

fn path_as_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::Config(format!(
            "managed repository path is not valid UTF-8: {}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tempfile::TempDir;

    use super::*;

    fn spec(source: &str, id: &str, remote: &str) -> RepoSpec {
        RepoSpec {
            id: format!("{source}/{id}"),
            source: source.into(),
            canonical_url: remote.into(),
            clone_url: remote.into(),
            default_branch: Some("main".into()),
            tags: BTreeSet::from(["rust".into()]),
        }
    }

    #[test]
    fn reconciliation_is_non_destructive_and_merges_sources() {
        let temp = TempDir::new().unwrap();
        let state = State::initialize(&temp.path().join("state")).unwrap();
        let repos = temp.path().join("repos");
        state
            .reconcile_source("one", &[spec("one", "org/repo", "host/org/repo")], &repos)
            .unwrap();
        state.mark_ready("one/org/repo").unwrap();
        state
            .reconcile_source("two", &[spec("two", "org/repo", "host/org/repo")], &repos)
            .unwrap();
        let record = state.get("one/org/repo").unwrap();
        assert_eq!(record.sources, BTreeSet::from(["one".into(), "two".into()]));

        state.reconcile_source("one", &[], &repos).unwrap();
        assert!(state.get("one/org/repo").unwrap().active);
        state.reconcile_source("two", &[], &repos).unwrap();
        assert!(!state.get("one/org/repo").unwrap().active);
    }
}
