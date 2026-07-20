use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use base64::Engine;
use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use multi_repo_core::git::SyncAction;
use multi_repo_core::search::{
    MatchEvent, RepoFilter, SearchEvent, SearchOptions, SearchOutputMode, search, select_repos,
};
use multi_repo_core::sync::{SyncOptions, synchronize};
use multi_repo_core::{Config, Error, State};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(
    name = "multi-repo",
    version,
    about = "Fast, safe multi-repository management"
)]
struct Cli {
    /// Configuration file (defaults to $XDG_CONFIG_HOME/multi-repo/config.toml)
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
    /// Control colored output
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
        Err(error) => {
            eprintln!("multi-repo: {error}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> multi_repo_core::Result<u8> {
    let cli = Cli::parse();
    let (config, _) = Config::load(cli.config.as_deref())?;
    let state = State::initialize(&config.state_dir())?;
    match cli.command {
        Command::Sync(args) => run_sync(&config, &state, &args).await,
        Command::List(args) => run_list(&state, &args),
        Command::Grep(args) => run_grep(&state, &args),
    }
}

async fn run_sync(config: &Config, state: &State, args: &SyncArgs) -> multi_repo_core::Result<u8> {
    let report = synchronize(
        config,
        state,
        SyncOptions {
            jobs: args.jobs,
            dry_run: args.dry_run,
        },
    )
    .await?;
    for source in &report.sources {
        if let Some(error) = &source.error {
            eprintln!("source {}: {error}", source.source);
        } else {
            println!(
                "source {}: {} repositories",
                source.source, source.discovered
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
    }
    Ok(u8::from(report.failed()) * 2)
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

fn run_grep(state: &State, args: &GrepArgs) -> multi_repo_core::Result<u8> {
    let repos = select_repos(state.searchable()?, &args.filter.as_filter())?;
    let context = args.context;
    let options = SearchOptions {
        pattern: args.pattern.clone(),
        fixed_strings: args.fixed_strings,
        ignore_case: args.ignore_case,
        smart_case: args.smart_case,
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
    let output = Arc::new(Output::new(args.json, use_color, args.sort_path));
    let callback_output = Arc::clone(&output);
    let emit: Arc<dyn Fn(SearchEvent) + Send + Sync> =
        Arc::new(move |event| callback_output.handle(event));
    let stats = search(&repos, &options, &emit)?;
    output.finish()?;
    if stats.errors > 0 {
        Ok(2)
    } else if stats.matches > 0 {
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
    stdout: Mutex<io::Stdout>,
}

impl Output {
    fn new(json: bool, color: bool, sort: bool) -> Self {
        Self {
            json,
            color,
            sort,
            buffered: Mutex::new(Vec::new()),
            stdout: Mutex::new(io::stdout()),
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
            if let Ok(mut buffered) = self.buffered.lock() {
                buffered.push(event);
            }
        } else if let Ok(mut stdout) = self.stdout.lock()
            && let Err(error) = write_event(&mut *stdout, &event, self.json, self.color)
        {
            eprintln!("failed to write search output: {error}");
        }
    }

    fn finish(&self) -> multi_repo_core::Result<()> {
        if !self.sort {
            return Ok(());
        }
        let mut events = self
            .buffered
            .lock()
            .map_err(|_| Error::Task("output buffer lock poisoned".into()))?;
        events.sort_by_key(event_sort_key);
        let mut stdout = self
            .stdout
            .lock()
            .map_err(|_| Error::Task("stdout lock poisoned".into()))?;
        for event in &*events {
            write_event(&mut *stdout, event, self.json, self.color).map_err(|source| {
                Error::Write {
                    path: PathBuf::from("<stdout>"),
                    source,
                }
            })?;
        }
        Ok(())
    }
}

fn event_sort_key(event: &SearchEvent) -> (String, PathBuf, u64) {
    match event {
        SearchEvent::Match(found) => (found.repo.clone(), found.path.clone(), found.line),
        SearchEvent::File { repo, path } | SearchEvent::Count { repo, path, .. } => {
            (repo.clone(), path.clone(), 0)
        }
        SearchEvent::Repo { repo } => (repo.clone(), PathBuf::new(), 0),
        SearchEvent::Error { path, .. } => (String::new(), path.clone().unwrap_or_default(), 0),
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
    output.write_all(&found.text)?;
    output.write_all(b"\n")
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
