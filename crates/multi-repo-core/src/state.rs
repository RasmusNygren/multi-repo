use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};

use crate::config::validate_repo_name;
use crate::error::{Error, Result};
use crate::model::{RepoRecord, RepoSpec, RepoStatus};

const SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug)]
pub struct State {
    path: PathBuf,
}

impl State {
    /// Opens or creates the state database and applies the current schema.
    ///
    /// # Errors
    ///
    /// Returns an error if the state directory cannot be created, the database
    /// cannot be opened or initialized, or its schema is unsupported.
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

    pub(crate) fn list_existing(state_dir: &Path) -> Result<Vec<RepoRecord>> {
        let path = state_dir.join("state.sqlite3");
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(source) => return Err(Error::Read { path, source }),
        }
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Err(Error::Config(format!(
                "unsupported state schema {version}; expected {SCHEMA_VERSION}"
            )));
        }
        list_repositories(&connection, true)
    }

    /// Reconciles one successful source discovery with the stored inventory.
    ///
    /// Existing repositories and source associations are retained but marked
    /// inactive when they are no longer discovered.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid repository data, conflicting identities,
    /// non-UTF-8 managed paths, serialization failures, or database failures.
    pub(crate) fn reconcile_source(
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
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = ids.into_iter().collect::<BTreeSet<_>>();
        Ok(self
            .list(true)?
            .into_iter()
            .filter(|repo| ids.contains(&repo.id))
            .collect())
    }

    /// Marks a repository ready and clears its last synchronization error.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub(crate) fn mark_ready(&self, id: &str) -> Result<()> {
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

    /// Records a synchronization error, optionally retaining ready status.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub(crate) fn mark_error(&self, id: &str, error: &str, preserve_ready: bool) -> Result<()> {
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

    pub(crate) fn remove_inactive(&self, id: &str) -> Result<()> {
        let connection = self.connect()?;
        let removed = connection.execute("DELETE FROM repos WHERE id = ?1 AND active = 0", [id])?;
        if removed == 1 {
            Ok(())
        } else {
            Err(Error::Task(format!(
                "repository {id:?} is no longer inactive"
            )))
        }
    }

    /// Lists repositories and their active source associations in ID order.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be queried or stored tags
    /// cannot be decoded.
    pub fn list(&self, include_inactive: bool) -> Result<Vec<RepoRecord>> {
        let connection = self.connect()?;
        list_repositories(&connection, include_inactive)
    }

    /// Lists active, ready repositories whose working trees still exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the inventory cannot be loaded.
    pub fn searchable(&self) -> Result<Vec<RepoRecord>> {
        Ok(self
            .list(false)?
            .into_iter()
            .filter(|repo| repo.status == RepoStatus::Ready && repo.local_path.is_dir())
            .collect())
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

fn list_repositories(connection: &Connection, include_inactive: bool) -> Result<Vec<RepoRecord>> {
    let sql = if include_inactive {
        "SELECT id, canonical_url, clone_url, local_path, default_branch,
                active, status, last_error
         FROM repos ORDER BY id"
    } else {
        "SELECT id, canonical_url, clone_url, local_path, default_branch,
                active, status, last_error
         FROM repos WHERE active = 1 ORDER BY id"
    };
    let mut records = {
        let mut statement = connection.prepare(sql)?;
        statement
            .query_map([], repo_from_row)?
            .map(|result| result.map(|repo| (repo.id.clone(), repo)))
            .collect::<std::result::Result<BTreeMap<_, _>, _>>()?
    };
    let mut statement = connection.prepare(
        "SELECT repo_id, source, tags_json FROM repo_sources
         WHERE active = 1 ORDER BY repo_id, source",
    )?;
    let associations = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for association in associations {
        let (repo_id, source, tags) = association?;
        let Some(record) = records.get_mut(&repo_id) else {
            continue;
        };
        record.sources.insert(source);
        record
            .tags
            .extend(serde_json::from_str::<BTreeSet<String>>(&tags)?);
    }
    Ok(records.into_values().collect())
}

fn repo_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RepoRecord> {
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
            canonical_url: remote.into(),
            clone_url: remote.into(),
            default_branch: Some("main".into()),
            tags: BTreeSet::from(["rust".into()]),
        }
    }

    fn find_record(state: &State, id: &str) -> RepoRecord {
        state
            .list(true)
            .unwrap()
            .into_iter()
            .find(|repo| repo.id == id)
            .unwrap()
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
        let repo = find_record(&state, "one/org/repo");
        assert_eq!(repo.sources, BTreeSet::from(["one".into(), "two".into()]));

        state.reconcile_source("one", &[], &repos).unwrap();
        assert!(find_record(&state, "one/org/repo").active);
        state.reconcile_source("two", &[], &repos).unwrap();
        assert!(!find_record(&state, "one/org/repo").active);
    }

    #[test]
    fn existing_inventory_can_be_read_without_creating_one() {
        let temp = TempDir::new().unwrap();
        let state_dir = temp.path().join("state");
        assert!(State::list_existing(&state_dir).unwrap().is_empty());
        assert!(!state_dir.exists());

        let state = State::initialize(&state_dir).unwrap();
        state
            .reconcile_source(
                "one",
                &[spec("one", "org/repo", "host/org/repo")],
                &temp.path().join("repos"),
            )
            .unwrap();

        let records = State::list_existing(&state_dir).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "one/org/repo");
    }
}
