use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::{Error, Result};
use crate::git::{default_branch, git_optional_stdout, git_stdout};
use crate::model::RepoRecord;

const FIELD_SEPARATOR: char = '\x1f';
// Subject is last so it may contain anything except the NUL record separator.
const FORMAT: &str =
    "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI%x1f%ct%x1f%cd%x1f%s";
const FIELDS: usize = 11;

#[derive(Clone, Debug, Default)]
pub struct LogOptions {
    /// Include commits whose committer date is at or after this Git date.
    pub since: Option<String>,
    /// Include commits whose committer date is at or before this Git date.
    pub until: Option<String>,
    /// Include commits whose author matches any of these patterns.
    pub authors: Vec<String>,
    /// Include commits whose message matches any of these patterns.
    pub grep: Vec<String>,
    /// Maximum concurrent Git processes.
    pub jobs: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    pub repo: String,
    pub hash: String,
    pub short_hash: String,
    pub author_name: String,
    pub author_email: String,
    /// Strict ISO 8601 author date in the author's time zone.
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    /// Strict ISO 8601 committer date in the committer's time zone.
    pub commit_date: String,
    /// Committer date as Unix seconds.
    pub commit_timestamp: i64,
    /// Committer date as `YYYY-MM-DD HH:MM` in the local time zone.
    pub local_commit_date: String,
    pub subject: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoLogError {
    pub repo: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LogReport {
    /// Commits from every repository, newest committer date first.
    pub commits: Vec<Commit>,
    pub errors: Vec<RepoLogError>,
}

impl LogReport {
    #[must_use]
    pub fn failed(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Lists commits on each repository's fetched default branch.
///
/// Repositories are read concurrently, and their commits are merged into one
/// timeline ordered by committer date. Per-repository failures are reported
/// in [`LogReport::errors`] without discarding commits from other
/// repositories.
#[must_use]
pub fn log(repos: &[RepoRecord], options: &LogOptions) -> LogReport {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(repos.len()));
    std::thread::scope(|scope| {
        for _ in 0..options.jobs.clamp(1, repos.len().max(1)) {
            scope.spawn(|| {
                while let Some(repo) = repos.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let result = repo_log(repo, options);
                    if let Ok(mut results) = results.lock() {
                        results.push((repo.id.clone(), result));
                    }
                }
            });
        }
    });
    let mut report = LogReport::default();
    for (repo, result) in results.into_inner().unwrap_or_default() {
        match result {
            Ok(commits) => report.commits.extend(commits),
            Err(error) => report.errors.push(RepoLogError {
                repo,
                message: error.to_string(),
            }),
        }
    }
    // The stable sort keeps Git's order for commits sharing a timestamp.
    report.commits.sort_by(|left, right| {
        right
            .commit_timestamp
            .cmp(&left.commit_timestamp)
            .then_with(|| left.repo.cmp(&right.repo))
    });
    report
        .errors
        .sort_by(|left, right| left.repo.cmp(&right.repo));
    report
}

fn repo_log(repo: &RepoRecord, options: &LogOptions) -> Result<Vec<Commit>> {
    let branch = default_branch(repo)?
        .ok_or_else(|| Error::Git("default branch is unknown; run sync to detect it".into()))?;
    let revision = format!("refs/remotes/origin/{branch}");
    let verify = format!("{revision}^{{commit}}");
    if git_optional_stdout(
        &repo.local_path,
        ["rev-parse", "--verify", "--quiet", &verify],
    )?
    .is_none()
    {
        return Err(Error::Git(format!(
            "origin/{branch} has not been fetched; run sync"
        )));
    }
    let mut args = vec![
        "log".to_owned(),
        "-z".to_owned(),
        "--no-show-signature".to_owned(),
        "--date=format-local:%Y-%m-%d %H:%M".to_owned(),
        FORMAT.to_owned(),
    ];
    if let Some(since) = &options.since {
        args.push(format!("--since={since}"));
    }
    if let Some(until) = &options.until {
        args.push(format!("--until={until}"));
    }
    args.extend(
        options
            .authors
            .iter()
            .map(|author| format!("--author={author}")),
    );
    args.extend(
        options
            .grep
            .iter()
            .map(|pattern| format!("--grep={pattern}")),
    );
    args.push(revision);
    args.push("--".to_owned());
    parse_log(&repo.id, &git_stdout(&repo.local_path, &args)?)
}

fn parse_log(repo: &str, output: &str) -> Result<Vec<Commit>> {
    output
        .split_terminator('\0')
        .map(|record| parse_commit(repo, record))
        .collect()
}

fn parse_commit(repo: &str, record: &str) -> Result<Commit> {
    let fields: Vec<_> = record.splitn(FIELDS, FIELD_SEPARATOR).collect();
    let [
        hash,
        short_hash,
        author_name,
        author_email,
        author_date,
        committer_name,
        committer_email,
        commit_date,
        commit_timestamp,
        local_commit_date,
        subject,
    ] = fields.as_slice()
    else {
        return Err(Error::Git(format!("unexpected git log record {record:?}")));
    };
    let commit_timestamp = commit_timestamp
        .parse()
        .map_err(|_| Error::Git(format!("invalid commit timestamp {commit_timestamp:?}")))?;
    Ok(Commit {
        repo: repo.to_owned(),
        hash: (*hash).to_owned(),
        short_hash: (*short_hash).to_owned(),
        author_name: (*author_name).to_owned(),
        author_email: (*author_email).to_owned(),
        author_date: (*author_date).to_owned(),
        committer_name: (*committer_name).to_owned(),
        committer_email: (*committer_email).to_owned(),
        commit_date: (*commit_date).to_owned(),
        commit_timestamp,
        local_commit_date: (*local_commit_date).to_owned(),
        subject: (*subject).to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_separated_records_with_unusual_subjects() {
        let output = "aaaa\x1faa\x1fAda\x1fada@example.com\x1f2026-01-01T12:00:00+01:00\x1f\
                      Bob\x1fbob@example.com\x1f2026-01-02T12:00:00Z\x1f1767355200\x1f\
                      2026-01-02 13:00\x1fFix (#12) \x1f with\ttabs\0\
                      bbbb\x1fbb\x1fAda\x1fada@example.com\x1f2026-01-01T12:00:00Z\x1f\
                      Ada\x1fada@example.com\x1f2026-01-01T12:00:00Z\x1f1767268800\x1f\
                      2026-01-01 13:00\x1f\0";

        let commits = parse_log("acme/repo", output).unwrap();

        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].repo, "acme/repo");
        assert_eq!(commits[0].short_hash, "aa");
        assert_eq!(commits[0].author_name, "Ada");
        assert_eq!(commits[0].committer_name, "Bob");
        assert_eq!(commits[0].commit_timestamp, 1_767_355_200);
        assert_eq!(commits[0].subject, "Fix (#12) \x1f with\ttabs");
        assert_eq!(commits[1].subject, "");
    }

    #[test]
    fn rejects_truncated_records() {
        assert!(parse_log("acme/repo", "aaaa\x1faa\0").is_err());
    }
}
