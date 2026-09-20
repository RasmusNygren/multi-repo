use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use globset::GlobSet;
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{DirEntry, WalkBuilder, WalkState};

use crate::error::{Error, Result};
use crate::model::RepoRecord;
use crate::provider::build_globs;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchOutputMode {
    #[default]
    Matches,
    FilesWithMatches,
    ReposWithMatches,
    Count,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PatternKind {
    #[default]
    Regex,
    Fixed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CaseMode {
    #[default]
    Sensitive,
    Insensitive,
    Smart,
}

#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    pub pattern: String,
    pub pattern_kind: PatternKind,
    pub case: CaseMode,
    pub word: bool,
    pub before_context: usize,
    pub after_context: usize,
    pub globs: Vec<String>,
    pub path_prefixes: Vec<PathBuf>,
    pub threads: usize,
    pub output_mode: SearchOutputMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SearchEvent {
    Match(MatchEvent),
    File {
        repo: String,
        path: PathBuf,
    },
    Repo {
        repo: String,
    },
    Count {
        repo: String,
        path: PathBuf,
        count: u64,
    },
    Error {
        path: Option<PathBuf>,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchEvent {
    pub repo: String,
    pub path: PathBuf,
    pub line: u64,
    pub column: Option<usize>,
    pub text: Vec<u8>,
    pub submatches: Vec<Submatch>,
    pub context: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Submatch {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SearchStats {
    pub matches: u64,
    pub errors: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RepoFilter {
    pub repo_globs: Vec<String>,
    pub sources: HashSet<String>,
    pub tags: HashSet<String>,
}

/// Selects repositories matching every non-empty filter category.
///
/// # Errors
///
/// Returns an error if a repository glob is invalid.
pub fn select_repos(repos: Vec<RepoRecord>, filter: &RepoFilter) -> Result<Vec<RepoRecord>> {
    let repo_globs = build_globs(&filter.repo_globs)?;
    Ok(repos
        .into_iter()
        .filter(|repo| {
            (filter.repo_globs.is_empty() || repo_globs.is_match(&repo.id))
                && (filter.sources.is_empty()
                    || repo
                        .sources
                        .iter()
                        .any(|source| filter.sources.contains(source)))
                && (filter.tags.is_empty() || repo.tags.iter().any(|tag| filter.tags.contains(tag)))
        })
        .collect())
}

/// Searches all selected working trees and emits results from worker threads.
///
/// # Errors
///
/// Returns an error if the search pattern or a path glob is invalid. File
/// traversal and read errors are emitted as [`SearchEvent::Error`] values and
/// counted in the returned statistics.
pub fn search(
    repos: &[RepoRecord],
    options: &SearchOptions,
    emit: &(dyn Fn(SearchEvent) + Sync),
) -> Result<SearchStats> {
    if repos.is_empty() {
        return Ok(SearchStats::default());
    }
    let roots: HashMap<_, _> = repos
        .iter()
        .cloned()
        .map(|repo| (repo.local_path.clone(), repo))
        .collect();
    let matcher = build_matcher(options)?;
    let path_filter = PathFilter::new(&options.globs, &options.path_prefixes)?;
    let reported_repos = Mutex::new(HashSet::<String>::new());
    let match_count = AtomicU64::new(0);
    let errors = AtomicU64::new(0);
    let mut walker = WalkBuilder::empty();
    for repo in repos {
        walker.add(&repo.local_path);
    }
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(true)
        .threads(options.threads)
        .filter_entry(|entry| entry.file_name() != ".git");
    walker.build_parallel().run(|| {
        let roots = &roots;
        let matcher = &matcher;
        let path_filter = &path_filter;
        let reported_repos = &reported_repos;
        let match_count = &match_count;
        let errors = &errors;
        let output_mode = options.output_mode;
        let mut searcher = build_searcher(options.before_context, options.after_context);
        Box::new(move |entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    emit(SearchEvent::Error {
                        path: None,
                        message: error.to_string(),
                    });
                    return WalkState::Continue;
                }
            };
            if !is_file(&entry) {
                return WalkState::Continue;
            }
            let Some(repo) = find_repo(entry.path(), roots) else {
                return WalkState::Continue;
            };
            let Ok(relative) = entry.path().strip_prefix(&repo.local_path) else {
                return WalkState::Continue;
            };
            if output_mode == SearchOutputMode::ReposWithMatches
                && reported_repos
                    .lock()
                    .is_ok_and(|set| set.contains(&repo.id))
            {
                return WalkState::Continue;
            }
            if !path_filter.matches(relative) {
                return WalkState::Continue;
            }
            let sink = EventSink {
                repo: &repo.id,
                path: relative,
                matcher,
                mode: output_mode,
                reported_repos,
                matches: match_count,
                emit,
                count: 0,
            };
            if let Err(error) = searcher.search_path(matcher, entry.path(), sink) {
                errors.fetch_add(1, Ordering::Relaxed);
                emit(SearchEvent::Error {
                    path: Some(entry.path().to_path_buf()),
                    message: error.to_string(),
                });
            }
            WalkState::Continue
        })
    });

    Ok(SearchStats {
        matches: match_count.load(Ordering::Relaxed),
        errors: errors.load(Ordering::Relaxed),
    })
}

fn build_searcher(before_context: usize, after_context: usize) -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .before_context(before_context)
        .after_context(after_context)
        .binary_detection(BinaryDetection::quit(b'\0'))
        .build()
}

fn build_matcher(options: &SearchOptions) -> Result<RegexMatcher> {
    let mut builder = RegexMatcherBuilder::new();
    match options.case {
        CaseMode::Sensitive => {}
        CaseMode::Insensitive => {
            builder.case_insensitive(true);
        }
        CaseMode::Smart => {
            builder.case_smart(true);
        }
    }
    builder.word(options.word);
    let result = match options.pattern_kind {
        PatternKind::Regex => builder.build(&options.pattern),
        PatternKind::Fixed => builder.build_literals(&[&options.pattern]),
    };
    result.map_err(|error| Error::Search(error.to_string()))
}

fn find_repo<'a>(path: &Path, roots: &'a HashMap<PathBuf, RepoRecord>) -> Option<&'a RepoRecord> {
    path.ancestors().find_map(|ancestor| roots.get(ancestor))
}

fn is_file(entry: &DirEntry) -> bool {
    entry
        .file_type()
        .map_or_else(|| entry.path().is_file(), |kind| kind.is_file())
}

struct PathFilter {
    include: GlobSet,
    exclude: GlobSet,
    has_include: bool,
    prefixes: Vec<PathBuf>,
}

impl PathFilter {
    fn new(globs: &[String], prefixes: &[PathBuf]) -> Result<Self> {
        let includes = globs
            .iter()
            .filter(|glob| !glob.starts_with('!'))
            .cloned()
            .collect::<Vec<_>>();
        let excludes = globs
            .iter()
            .filter_map(|glob| glob.strip_prefix('!').map(str::to_owned))
            .collect::<Vec<_>>();
        Ok(Self {
            include: build_globs(&includes)?,
            exclude: build_globs(&excludes)?,
            has_include: !includes.is_empty(),
            prefixes: prefixes.to_vec(),
        })
    }

    fn matches(&self, path: &Path) -> bool {
        (self.prefixes.is_empty() || self.prefixes.iter().any(|prefix| path.starts_with(prefix)))
            && (!self.has_include || self.include.is_match(path))
            && !self.exclude.is_match(path)
    }
}

struct EventSink<'a> {
    repo: &'a str,
    path: &'a Path,
    matcher: &'a RegexMatcher,
    mode: SearchOutputMode,
    reported_repos: &'a Mutex<HashSet<String>>,
    matches: &'a AtomicU64,
    emit: &'a (dyn Fn(SearchEvent) + Sync),
    count: u64,
}

impl Sink for EventSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        matched: &SinkMatch<'_>,
    ) -> std::result::Result<bool, Self::Error> {
        self.count += 1;
        self.matches.fetch_add(1, Ordering::Relaxed);
        match self.mode {
            SearchOutputMode::Matches => {
                let bytes = trim_line_ending(matched.bytes());
                let mut submatches = Vec::new();
                self.matcher
                    .find_iter(bytes, |found| {
                        submatches.push(Submatch {
                            start: found.start(),
                            end: found.end(),
                        });
                        true
                    })
                    .map_err(std::io::Error::other)?;
                let column = submatches.first().map(|found| found.start + 1);
                (self.emit)(SearchEvent::Match(MatchEvent {
                    repo: self.repo.to_owned(),
                    path: self.path.to_path_buf(),
                    line: matched.line_number().unwrap_or(0),
                    column,
                    text: bytes.to_vec(),
                    submatches,
                    context: false,
                }));
                Ok(true)
            }
            SearchOutputMode::FilesWithMatches => {
                (self.emit)(SearchEvent::File {
                    repo: self.repo.to_owned(),
                    path: self.path.to_path_buf(),
                });
                Ok(false)
            }
            SearchOutputMode::ReposWithMatches => {
                let first_match = self
                    .reported_repos
                    .lock()
                    .map_err(|_| std::io::Error::other("repository result lock poisoned"))?
                    .insert(self.repo.to_owned());
                if first_match {
                    (self.emit)(SearchEvent::Repo {
                        repo: self.repo.to_owned(),
                    });
                }
                Ok(false)
            }
            SearchOutputMode::Count => Ok(true),
        }
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        context: &SinkContext<'_>,
    ) -> std::result::Result<bool, Self::Error> {
        if self.mode == SearchOutputMode::Matches {
            (self.emit)(SearchEvent::Match(MatchEvent {
                repo: self.repo.to_owned(),
                path: self.path.to_path_buf(),
                line: context.line_number().unwrap_or(0),
                column: None,
                text: trim_line_ending(context.bytes()).to_vec(),
                submatches: Vec::new(),
                context: true,
            }));
        }
        Ok(true)
    }

    fn finish(
        &mut self,
        _searcher: &Searcher,
        _finish: &grep_searcher::SinkFinish,
    ) -> std::result::Result<(), Self::Error> {
        if self.mode == SearchOutputMode::Count && self.count > 0 {
            (self.emit)(SearchEvent::Count {
                repo: self.repo.to_owned(),
                path: self.path.to_path_buf(),
                count: self.count,
            });
        }
        Ok(())
    }
}

fn trim_line_ending(mut bytes: &[u8]) -> &[u8] {
    if bytes.ends_with(b"\n") {
        bytes = &bytes[..bytes.len() - 1];
    }
    if bytes.ends_with(b"\r") {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use tempfile::TempDir;

    use super::*;
    use crate::model::RepoStatus;

    #[test]
    fn searches_dotfiles_and_untracked_but_respects_ignores() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::create_dir_all(root.join("ignored")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(root.join("visible.txt"), "needle here\n").unwrap();
        std::fs::write(root.join(".github/workflow.yml"), "needle there\n").unwrap();
        std::fs::write(root.join("ignored/no.txt"), "needle hidden\n").unwrap();
        std::fs::write(root.join(".git/config"), "needle metadata\n").unwrap();
        let repo = RepoRecord {
            id: "manual/repo".into(),
            canonical_url: "host/repo".into(),
            clone_url: "host/repo".into(),
            local_path: root,
            default_branch: Some("main".into()),
            active: true,
            status: RepoStatus::Ready,
            sources: BTreeSet::from(["manual".into()]),
            tags: BTreeSet::new(),
            last_error: None,
        };
        let events = Mutex::new(Vec::new());
        let emit = |event| events.lock().unwrap().push(event);
        let stats = search(
            &[repo],
            &SearchOptions {
                pattern: "needle".into(),
                ..SearchOptions::default()
            },
            &emit,
        )
        .unwrap();
        assert_eq!(stats.matches, 2);
        let events = events.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            SearchEvent::Match(found) if found.path == Path::new(".github/workflow.yml")
        )));
    }
}
