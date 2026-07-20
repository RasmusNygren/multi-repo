use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use globset::{Glob, GlobSet, GlobSetBuilder};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{DirEntry, WalkBuilder, WalkState};

use crate::error::{Error, Result};
use crate::model::RepoRecord;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchOutputMode {
    #[default]
    Matches,
    FilesWithMatches,
    ReposWithMatches,
    Count,
}

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub pattern: String,
    pub fixed_strings: bool,
    pub ignore_case: bool,
    pub smart_case: bool,
    pub word: bool,
    pub before_context: usize,
    pub after_context: usize,
    pub globs: Vec<String>,
    pub path_prefixes: Vec<PathBuf>,
    pub threads: usize,
    pub output_mode: SearchOutputMode,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            fixed_strings: false,
            ignore_case: false,
            smart_case: false,
            word: false,
            before_context: 0,
            after_context: 0,
            globs: Vec::new(),
            path_prefixes: Vec::new(),
            threads: 0,
            output_mode: SearchOutputMode::Matches,
        }
    }
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

pub type FileVisitor = Arc<dyn Fn(&RepoRecord, &Path, &Path) + Send + Sync>;
pub type FileWalkErrorVisitor = Arc<dyn Fn(Option<PathBuf>, String) + Send + Sync>;

/// Defines the set of files made available to the search engine.
///
/// A future tracked-files implementation can read Git indexes and invoke the
/// same visitor without changing matching, output, or repository filtering.
pub trait FileUniverse: Send + Sync {
    fn visit(
        &self,
        repos: &[RepoRecord],
        threads: usize,
        visitor: &FileVisitor,
        error_visitor: &FileWalkErrorVisitor,
    );
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkingTrees;

impl FileUniverse for WorkingTrees {
    fn visit(
        &self,
        repos: &[RepoRecord],
        threads: usize,
        visitor: &FileVisitor,
        error_visitor: &FileWalkErrorVisitor,
    ) {
        let roots: Arc<HashMap<PathBuf, Arc<RepoRecord>>> = Arc::new(
            repos
                .iter()
                .cloned()
                .map(|repo| (repo.local_path.clone(), Arc::new(repo)))
                .collect(),
        );
        let mut walker = WalkBuilder::empty();
        for repo in repos {
            walker.add(&repo.local_path);
        }
        walker
            .hidden(false)
            .follow_links(false)
            .require_git(true)
            .threads(threads)
            .filter_entry(|entry| entry.file_name() != ".git");
        walker.build_parallel().run(|| {
            let roots = Arc::clone(&roots);
            let visitor = Arc::clone(visitor);
            let error_visitor = Arc::clone(error_visitor);
            Box::new(move |entry| {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        error_visitor(None, error.to_string());
                        return WalkState::Continue;
                    }
                };
                if !is_file(&entry) {
                    return WalkState::Continue;
                }
                let Some(repo) = find_repo(entry.path(), &roots) else {
                    return WalkState::Continue;
                };
                let Ok(relative) = entry.path().strip_prefix(&repo.local_path) else {
                    return WalkState::Continue;
                };
                visitor(repo, entry.path(), relative);
                WalkState::Continue
            })
        });
    }
}

pub fn select_repos(repos: Vec<RepoRecord>, filter: &RepoFilter) -> Result<Vec<RepoRecord>> {
    let repo_globs = build_glob_set(&filter.repo_globs)?;
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

pub fn search(
    repos: &[RepoRecord],
    options: &SearchOptions,
    emit: &Arc<dyn Fn(SearchEvent) + Send + Sync>,
) -> Result<SearchStats> {
    search_files(&WorkingTrees, repos, options, emit)
}

pub fn search_files(
    universe: &dyn FileUniverse,
    repos: &[RepoRecord],
    options: &SearchOptions,
    emit: &Arc<dyn Fn(SearchEvent) + Send + Sync>,
) -> Result<SearchStats> {
    if repos.is_empty() {
        return Ok(SearchStats::default());
    }
    let matcher = Arc::new(build_matcher(options)?);
    let path_filter = Arc::new(PathFilter::new(&options.globs, &options.path_prefixes)?);
    let reported_repos = Arc::new(Mutex::new(HashSet::<String>::new()));
    let match_count = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let output_mode = options.output_mode;
    let before_context = options.before_context;
    let after_context = options.after_context;
    let file_visitor: FileVisitor = {
        let matcher = Arc::clone(&matcher);
        let path_filter = Arc::clone(&path_filter);
        let reported_repos = Arc::clone(&reported_repos);
        let match_count = Arc::clone(&match_count);
        let errors = Arc::clone(&errors);
        let emit = Arc::clone(emit);
        Arc::new(move |repo, absolute_path, relative| {
            if output_mode == SearchOutputMode::ReposWithMatches
                && reported_repos
                    .lock()
                    .is_ok_and(|set| set.contains(&repo.id))
            {
                return;
            }
            if !path_filter.matches(relative) {
                return;
            }
            let mut searcher = SearcherBuilder::new()
                .line_number(true)
                .before_context(before_context)
                .after_context(after_context)
                .binary_detection(BinaryDetection::quit(b'\0'))
                .build();
            let sink = EventSink {
                repo: &repo.id,
                path: relative,
                matcher: &matcher,
                mode: output_mode,
                reported_repos: &reported_repos,
                matches: &match_count,
                emit: &emit,
                count: 0,
            };
            if let Err(error) = searcher.search_path(&*matcher, absolute_path, sink) {
                errors.fetch_add(1, Ordering::Relaxed);
                emit(SearchEvent::Error {
                    path: Some(absolute_path.to_path_buf()),
                    message: error.to_string(),
                });
            }
        })
    };
    let error_visitor: FileWalkErrorVisitor = {
        let errors = Arc::clone(&errors);
        let emit = Arc::clone(emit);
        Arc::new(move |path, message| {
            errors.fetch_add(1, Ordering::Relaxed);
            emit(SearchEvent::Error { path, message });
        })
    };
    universe.visit(repos, options.threads, &file_visitor, &error_visitor);

    Ok(SearchStats {
        matches: match_count.load(Ordering::Relaxed),
        errors: errors.load(Ordering::Relaxed),
    })
}

fn build_matcher(options: &SearchOptions) -> Result<RegexMatcher> {
    let mut builder = RegexMatcherBuilder::new();
    builder
        .case_insensitive(options.ignore_case)
        .case_smart(options.smart_case)
        .word(options.word);
    let result = if options.fixed_strings {
        builder.build_literals(&[&options.pattern])
    } else {
        builder.build(&options.pattern)
    };
    result.map_err(|error| Error::Search(error.to_string()))
}

fn find_repo<'a>(
    path: &Path,
    roots: &'a HashMap<PathBuf, Arc<RepoRecord>>,
) -> Option<&'a Arc<RepoRecord>> {
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
            include: build_glob_set(&includes)?,
            exclude: build_glob_set(&excludes)?,
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

fn build_glob_set(globs: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(
            Glob::new(glob)
                .map_err(|error| Error::Config(format!("invalid glob {glob:?}: {error}")))?,
        );
    }
    builder
        .build()
        .map_err(|error| Error::Config(format!("invalid glob set: {error}")))
}

struct EventSink<'a> {
    repo: &'a str,
    path: &'a Path,
    matcher: &'a RegexMatcher,
    mode: SearchOutputMode,
    reported_repos: &'a Mutex<HashSet<String>>,
    matches: &'a AtomicU64,
    emit: &'a Arc<dyn Fn(SearchEvent) + Send + Sync>,
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
                let mut reported = self
                    .reported_repos
                    .lock()
                    .map_err(|_| std::io::Error::other("repository result lock poisoned"))?;
                if reported.insert(self.repo.to_owned()) {
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
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&events);
        let emit: Arc<dyn Fn(SearchEvent) + Send + Sync> =
            Arc::new(move |event| output.lock().unwrap().push(event));
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
