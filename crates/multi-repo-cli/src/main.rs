use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use base64::Engine;
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use multi_repo_core::prune::{PruneAction, PruneOptions, prune};
use multi_repo_core::search::{
    CaseMode, MatchEvent, PatternKind, RepoFilter, SearchEvent, SearchOptions, SearchOutputMode,
    Submatch, search, select_repos,
};
use multi_repo_core::sync::{
    RepositoryChange, RepositoryChangeKind, SyncOptions, SyncProgress, synchronize_with_progress,
};
use multi_repo_core::{Config, Error, State, SyncAction};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "multi-repo",
    version,
    about = "Fast, safe multi-repository management"
)]
struct Cli {
    /// Override the nearest .multi-repo.toml workspace configuration
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Discover, clone, fetch, and safely fast-forward repositories
    Sync(SyncArgs),
    /// List repositories in the local inventory
    List(ListArgs),
    /// Delete inactive repositories with clean working trees
    Prune(PruneArgs),
    /// Search all selected repository working trees
    Grep(GrepArgs),
}

#[derive(Debug, Args)]
struct SyncArgs {
    /// Discover repositories without changing local state
    #[arg(long)]
    dry_run: bool,
    /// Maximum concurrent Git operations
    #[arg(short = 'j', long, default_value_t = default_jobs())]
    jobs: usize,
    /// Control colored dry-run diff output
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto)]
    color: ColorChoice,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[command(flatten)]
    filter: FilterArgs,
    /// Include repositories no longer returned by a configured source
    #[arg(long)]
    all: bool,
    /// Emit a JSON array
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct PruneArgs {
    /// Report what would be removed without changing files or state
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Debug, Args)]
struct FilterArgs {
    /// Select repository IDs by glob; repeat for OR matching
    #[arg(long = "repo", value_name = "GLOB")]
    repos: Vec<String>,
    /// Select repositories associated with a source
    #[arg(long, value_name = "NAME")]
    source: Vec<String>,
    /// Select repositories carrying a tag
    #[arg(long, value_name = "TAG")]
    tag: Vec<String>,
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
#[command(group(
    clap::ArgGroup::new("result_mode")
        .args(["files_with_matches", "repos_with_matches", "count"])
        .multiple(false)
))]
struct GrepArgs {
    /// Regular expression or fixed string to search for
    pattern: String,
    #[command(flatten)]
    filter: FilterArgs,
    /// Treat PATTERN as a literal string
    #[arg(short = 'F', long)]
    fixed_strings: bool,
    /// Search case-insensitively
    #[arg(short = 'i', long, conflicts_with = "smart_case")]
    ignore_case: bool,
    /// Search case-insensitively unless PATTERN contains an uppercase literal
    #[arg(short = 'S', long, conflicts_with = "ignore_case")]
    smart_case: bool,
    /// Require matches to be surrounded by word boundaries
    #[arg(short = 'w', long)]
    word: bool,
    /// Include NUM lines before each match
    #[arg(short = 'B', long, default_value_t = 0)]
    before_context: usize,
    /// Include NUM lines after each match
    #[arg(short = 'A', long, default_value_t = 0)]
    after_context: usize,
    /// Include NUM lines before and after each match
    #[arg(short = 'C', long, value_name = "NUM")]
    context: Option<usize>,
    /// Include or exclude relative paths by glob; prefix exclusions with '!'
    #[arg(short = 'g', long = "glob", value_name = "GLOB", action = ArgAction::Append)]
    globs: Vec<String>,
    /// Print only files containing a match
    #[arg(short = 'l', long)]
    files_with_matches: bool,
    /// Print only repositories containing a match
    #[arg(long)]
    repos_with_matches: bool,
    /// Print matching-line counts per file
    #[arg(short = 'c', long)]
    count: bool,
    /// Emit JSON Lines
    #[arg(long)]
    json: bool,
    /// Buffer and sort results by repository and path
    #[arg(long)]
    sort_path: bool,
    /// Control colored paths and matches
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto)]
    color: ColorChoice,
    /// Search worker threads; zero chooses automatically
    #[arg(short = 'j', long, default_value_t = 0)]
    threads: usize,
    /// Limit the search to these paths relative to every repository
    #[arg(last = true, value_name = "PATH")]
    paths: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => ExitCode::from(code),
        Err(Error::Write { source, .. }) if source.kind() == io::ErrorKind::BrokenPipe => {
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("multi-repo: {error}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> multi_repo_core::Result<u8> {
    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Sync(args) => run_sync(&config, &args).await,
        Command::List(args) => run_list(&State::initialize(&config.state_dir())?, &args),
        Command::Prune(args) => run_prune(&config, &args),
        Command::Grep(args) => run_grep(&State::initialize(&config.state_dir())?, &args),
    }
}

async fn run_sync(config: &Config, args: &SyncArgs) -> multi_repo_core::Result<u8> {
    let mut progress = SyncProgressDisplay::new();
    let result = synchronize_with_progress(
        config,
        SyncOptions {
            jobs: args.jobs,
            dry_run: args.dry_run,
        },
        |event| progress.update(event),
    )
    .await;
    progress.finish();
    let report = result?;
    for source in &report.sources {
        if let Some(error) = &source.error {
            eprintln!("source {}: {error}", source.source);
        } else {
            let repository = if source.discovered == 1 {
                "repository"
            } else {
                "repositories"
            };
            println!(
                "source {}: {} {repository}",
                source.source, source.discovered,
            );
        }
    }
    for repo in &report.repos {
        if let Some(error) = &repo.error {
            eprintln!("{}: {error}", repo.id);
            continue;
        }
        let action = match repo.action {
            Some(SyncAction::Cloned) => "cloned",
            Some(SyncAction::FastForwarded) => "fast-forwarded",
            Some(SyncAction::Fetched) => "fetched",
            Some(SyncAction::Unchanged) => "up-to-date",
            None => "unknown",
        };
        if let Some(detail) = &repo.detail {
            println!("{}: {action} ({detail})", repo.id);
        } else {
            println!("{}: {action}", repo.id);
        }
        if let Some(warning) = &repo.warning {
            eprintln!("{}: warning: {warning}", repo.id);
        }
    }
    if let Some(preview) = &report.preview {
        if preview.changes.is_empty() {
            println!("repository changes: none");
        } else {
            println!("repository changes:");
            let color = match args.color {
                ColorChoice::Auto => io::stdout().is_terminal(),
                ColorChoice::Always => true,
                ColorChoice::Never => false,
            };
            for change in &preview.changes {
                println!("{}", repository_change_line(change, color));
            }
            if preview.unchanged > 0 {
                println!("    {} unchanged", preview.unchanged);
            }
        }
    }
    Ok(if report.failed() { 2 } else { 0 })
}

fn repository_change_line(change: &RepositoryChange, color: bool) -> String {
    let (marker, ansi) = match change.kind {
        RepositoryChangeKind::Activate => ('+', "\x1b[32m"),
        RepositoryChangeKind::Deactivate => ('-', "\x1b[31m"),
    };
    if color {
        format!("  {ansi}{marker} {}\x1b[0m", change.id)
    } else {
        format!("  {marker} {}", change.id)
    }
}

struct SyncProgressDisplay {
    enabled: bool,
}

impl SyncProgressDisplay {
    fn new() -> Self {
        Self {
            enabled: io::stderr().is_terminal(),
        }
    }

    fn update(&mut self, progress: SyncProgress) {
        if !self.enabled {
            return;
        }
        let mut stderr = io::stderr().lock();
        if write!(stderr, "\r\x1b[2K{}", progress_line(progress))
            .and_then(|()| stderr.flush())
            .is_err()
        {
            self.enabled = false;
        }
    }

    fn finish(&mut self) {
        if !self.enabled {
            return;
        }
        let mut stderr = io::stderr().lock();
        let _ = stderr.write_all(b"\r\x1b[2K").and_then(|()| stderr.flush());
        self.enabled = false;
    }
}

fn progress_line(progress: SyncProgress) -> String {
    const WIDTH: usize = 12;
    let (label, completed, total) = match progress {
        SyncProgress::Discovering { completed, total } => ("discover", completed, total),
        SyncProgress::Repositories { completed, total } => ("sync", completed, total),
    };
    let completed = completed.min(total);
    let filled = completed
        .saturating_mul(WIDTH)
        .checked_div(total)
        .unwrap_or(0);
    format!(
        "{label:<8}[{}{}] {completed}/{total}",
        "#".repeat(filled),
        "-".repeat(WIDTH - filled),
    )
}

fn run_list(state: &State, args: &ListArgs) -> multi_repo_core::Result<u8> {
    let repos = select_repos(state.list(args.all)?, &args.filter.as_filter())?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&repos)?);
    } else {
        for repo in repos {
            let inactive = if repo.active { "" } else { " (inactive)" };
            println!("{}{}\t{}", repo.id, inactive, repo.local_path.display());
        }
    }
    Ok(0)
}

fn run_prune(config: &Config, args: &PruneArgs) -> multi_repo_core::Result<u8> {
    let report = prune(
        config,
        PruneOptions {
            dry_run: args.dry_run,
        },
    )?;
    for repo in &report.repos {
        if let Some(error) = &repo.error {
            eprintln!("{}: {error}", repo.id);
            continue;
        }
        let action = match repo.action {
            Some(PruneAction::Deleted) => "deleted",
            Some(PruneAction::WouldDelete) => "would delete",
            Some(PruneAction::RemovedMissing) => "removed missing inventory entry",
            Some(PruneAction::WouldRemoveMissing) => "would remove missing inventory entry",
            Some(PruneAction::SkippedDirty) => "kept (working tree has local changes)",
            Some(PruneAction::SkippedLocalState) => {
                "kept (repository has local commits, stashes, or linked worktrees)"
            }
            None => "unknown",
        };
        println!("{}: {action}", repo.id);
    }
    Ok(if report.failed() { 2 } else { 0 })
}

fn run_grep(state: &State, args: &GrepArgs) -> multi_repo_core::Result<u8> {
    let repos = select_repos(state.searchable()?, &args.filter.as_filter())?;
    let context = args.context;
    let options = SearchOptions {
        pattern: args.pattern.clone(),
        pattern_kind: if args.fixed_strings {
            PatternKind::Fixed
        } else {
            PatternKind::Regex
        },
        case: if args.ignore_case {
            CaseMode::Insensitive
        } else if args.smart_case {
            CaseMode::Smart
        } else {
            CaseMode::Sensitive
        },
        word: args.word,
        before_context: context.unwrap_or(args.before_context),
        after_context: context.unwrap_or(args.after_context),
        globs: args.globs.clone(),
        path_prefixes: args.paths.clone(),
        threads: args.threads,
        output_mode: if args.files_with_matches {
            SearchOutputMode::FilesWithMatches
        } else if args.repos_with_matches {
            SearchOutputMode::ReposWithMatches
        } else if args.count {
            SearchOutputMode::Count
        } else {
            SearchOutputMode::Matches
        },
    };
    let use_color = !args.json
        && match args.color {
            ColorChoice::Auto => io::stdout().is_terminal(),
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        };
    let output = Output::new(args.json, use_color, args.sort_path);
    let emit = |event| output.handle(event);
    let search_stats = search(&repos, &options, &emit)?;
    output.finish()?;
    if search_stats.errors > 0 {
        Ok(2)
    } else if search_stats.matches > 0 {
        Ok(0)
    } else {
        Ok(1)
    }
}

impl FilterArgs {
    fn as_filter(&self) -> RepoFilter {
        RepoFilter {
            repo_globs: self.repos.clone(),
            sources: self.source.iter().cloned().collect(),
            tags: self.tag.iter().cloned().collect(),
        }
    }
}

struct Output {
    json: bool,
    color: bool,
    sort: bool,
    buffered: Mutex<Vec<SearchEvent>>,
    error: Mutex<Option<io::Error>>,
}

impl Output {
    fn new(json: bool, color: bool, sort: bool) -> Self {
        Self {
            json,
            color,
            sort,
            buffered: Mutex::new(Vec::new()),
            error: Mutex::new(None),
        }
    }

    fn handle(&self, event: SearchEvent) {
        if !self.json && matches!(event, SearchEvent::Error { .. }) {
            if let SearchEvent::Error { path, message } = event {
                if let Some(path) = path {
                    eprintln!("{}: {message}", path.display());
                } else {
                    eprintln!("{message}");
                }
            }
            return;
        }
        if self.sort {
            match self.buffered.lock() {
                Ok(mut buffered) => buffered.push(event),
                Err(_) => self.record_error(io::Error::other("output buffer lock poisoned")),
            }
        } else {
            let result = write_event(&mut io::stdout().lock(), &event, self.json, self.color);
            if let Err(error) = result {
                self.record_error(error);
            }
        }
    }

    fn finish(&self) -> multi_repo_core::Result<()> {
        if self.sort {
            let mut events = {
                let mut buffered = self
                    .buffered
                    .lock()
                    .map_err(|_| Error::Task("output buffer lock poisoned".into()))?;
                std::mem::take(&mut *buffered)
            };
            events.sort_by(compare_events);
            let mut stdout = io::stdout().lock();
            for event in &events {
                if let Err(error) = write_event(&mut stdout, event, self.json, self.color) {
                    self.record_error(error);
                    break;
                }
            }
        }
        if let Err(error) = io::stdout().flush() {
            self.record_error(error);
        }
        let error = self
            .error
            .lock()
            .map_err(|_| Error::Task("output error lock poisoned".into()))?
            .take();
        error.map_or(Ok(()), |source| {
            Err(Error::Write {
                path: PathBuf::from("<stdout>"),
                source,
            })
        })
    }

    fn record_error(&self, error: io::Error) {
        if let Ok(mut recorded) = self.error.lock()
            && recorded.is_none()
        {
            *recorded = Some(error);
        }
    }
}

fn compare_events(left: &SearchEvent, right: &SearchEvent) -> std::cmp::Ordering {
    event_sort_key(left).cmp(&event_sort_key(right))
}

fn event_sort_key(event: &SearchEvent) -> (&str, &Path, u64, u8) {
    match event {
        SearchEvent::Match(found) => (
            &found.repo,
            &found.path,
            found.line,
            u8::from(found.context),
        ),
        SearchEvent::File { repo, path } | SearchEvent::Count { repo, path, .. } => {
            (repo, path, 0, 2)
        }
        SearchEvent::Repo { repo } => (repo, Path::new(""), 0, 3),
        SearchEvent::Error { path, .. } => {
            ("", path.as_deref().unwrap_or_else(|| Path::new("")), 0, 4)
        }
    }
}

fn write_event(
    output: &mut dyn Write,
    event: &SearchEvent,
    json_output: bool,
    color: bool,
) -> io::Result<()> {
    if json_output {
        serde_json::to_writer(&mut *output, &json_event(event))?;
        output.write_all(b"\n")?;
        return Ok(());
    }
    match event {
        SearchEvent::Match(found) => write_match(output, found, color),
        SearchEvent::File { repo, path } => {
            write_prefix(output, repo, path, color)?;
            output.write_all(b"\n")
        }
        SearchEvent::Repo { repo } => writeln!(output, "{repo}"),
        SearchEvent::Count { repo, path, count } => {
            write_prefix(output, repo, path, color)?;
            writeln!(output, ":{count}")
        }
        SearchEvent::Error { .. } => Ok(()),
    }
}

fn write_match(output: &mut dyn Write, found: &MatchEvent, color: bool) -> io::Result<()> {
    write_prefix(output, &found.repo, &found.path, color)?;
    let separator = if found.context { '-' } else { ':' };
    write!(output, "{separator}{}", found.line)?;
    if let Some(column) = found.column {
        write!(output, ":{column}")?;
    }
    write!(output, "{separator}")?;
    if color && !found.context {
        write_highlighted_matches(output, &found.text, &found.submatches)?;
    } else {
        output.write_all(&found.text)?;
    }
    output.write_all(b"\n")
}

fn write_highlighted_matches(
    output: &mut dyn Write,
    text: &[u8],
    submatches: &[Submatch],
) -> io::Result<()> {
    let mut cursor = 0;
    for found in submatches {
        if found.start < cursor || found.start >= found.end || found.end > text.len() {
            continue;
        }
        output.write_all(&text[cursor..found.start])?;
        output.write_all(b"\x1b[1;31m")?;
        output.write_all(&text[found.start..found.end])?;
        output.write_all(b"\x1b[0m")?;
        cursor = found.end;
    }
    output.write_all(&text[cursor..])
}

fn write_prefix(output: &mut dyn Write, repo: &str, path: &Path, color: bool) -> io::Result<()> {
    if color {
        write!(output, "\x1b[36m{repo}\x1b[0m:")?;
        output.write_all(b"\x1b[32m")?;
        output.write_all(path_bytes(path))?;
        output.write_all(b"\x1b[0m")
    } else {
        write!(output, "{repo}:")?;
        output.write_all(path_bytes(path))
    }
}

fn json_event(event: &SearchEvent) -> Value {
    match event {
        SearchEvent::Match(found) => json!({
            "type": if found.context { "context" } else { "match" },
            "repo": found.repo,
            "path": json_bytes(path_bytes(&found.path)),
            "line": found.line,
            "column": found.column,
            "text": json_bytes(&found.text),
            "submatches": found.submatches.iter().map(|found| json!({
                "start": found.start,
                "end": found.end,
            })).collect::<Vec<_>>(),
        }),
        SearchEvent::File { repo, path } => json!({
            "type": "file",
            "repo": repo,
            "path": json_bytes(path_bytes(path)),
        }),
        SearchEvent::Repo { repo } => json!({"type": "repo", "repo": repo}),
        SearchEvent::Count { repo, path, count } => json!({
            "type": "count",
            "repo": repo,
            "path": json_bytes(path_bytes(path)),
            "count": count,
        }),
        SearchEvent::Error { path, message } => json!({
            "type": "error",
            "path": path.as_ref().map(|path| json_bytes(path_bytes(path))),
            "message": message,
        }),
    }
}

fn json_bytes(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({"text": text}),
        Err(_) => json!({
            "bytes": base64::engine::general_purpose::STANDARD.encode(bytes)
        }),
    }
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> &[u8] {
    path.to_str().unwrap_or("<non-UTF-8 path>").as_bytes()
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .min(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_are_small_and_bounded() {
        assert_eq!(
            progress_line(SyncProgress::Discovering {
                completed: 1,
                total: 2,
            }),
            "discover[######------] 1/2"
        );
        assert_eq!(
            progress_line(SyncProgress::Repositories {
                completed: 80,
                total: 60,
            }),
            "sync    [############] 60/60"
        );
    }

    #[test]
    fn repository_changes_use_diff_colors_when_enabled() {
        let added = RepositoryChange {
            id: "github/acme/new".into(),
            kind: RepositoryChangeKind::Activate,
        };
        let removed = RepositoryChange {
            id: "github/acme/old".into(),
            kind: RepositoryChangeKind::Deactivate,
        };

        assert_eq!(
            repository_change_line(&added, true),
            "  \x1b[32m+ github/acme/new\x1b[0m"
        );
        assert_eq!(
            repository_change_line(&removed, true),
            "  \x1b[31m- github/acme/old\x1b[0m"
        );
        assert_eq!(repository_change_line(&added, false), "  + github/acme/new");
    }

    #[test]
    fn match_output_highlights_every_match_when_color_is_enabled() {
        let found = MatchEvent {
            repo: "acme/widget".into(),
            path: PathBuf::from("README.md"),
            line: 4,
            column: Some(1),
            text: b"needle and needle".to_vec(),
            submatches: vec![
                Submatch { start: 0, end: 6 },
                Submatch { start: 11, end: 17 },
            ],
            context: false,
        };
        let mut output = Vec::new();

        write_match(&mut output, &found, true).unwrap();

        assert_eq!(
            output,
            b"\x1b[36macme/widget\x1b[0m:\x1b[32mREADME.md\x1b[0m:4:1:\x1b[1;31mneedle\x1b[0m and \x1b[1;31mneedle\x1b[0m\n"
        );
    }

    #[test]
    fn match_output_remains_plain_when_color_is_disabled() {
        let found = MatchEvent {
            repo: "acme/widget".into(),
            path: PathBuf::from("README.md"),
            line: 4,
            column: Some(1),
            text: b"needle".to_vec(),
            submatches: vec![Submatch { start: 0, end: 6 }],
            context: false,
        };
        let mut output = Vec::new();

        write_match(&mut output, &found, false).unwrap();

        assert_eq!(output, b"acme/widget:README.md:4:1:needle\n");
    }
}
