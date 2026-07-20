use std::collections::BTreeSet;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use multi_repo_core::RepoRecord;
use multi_repo_core::model::RepoStatus;
use multi_repo_core::search::{SearchEvent, SearchOptions, search};
use tempfile::TempDir;

fn main() -> multi_repo_core::Result<()> {
    let temporary = TempDir::new().expect("create benchmark directory");
    let mut repos = Vec::with_capacity(60);
    for repo_number in 0..60 {
        let root = temporary.path().join(format!("repo-{repo_number:02}"));
        fs::create_dir_all(root.join(".git")).expect("create git marker");
        fs::write(root.join(".gitignore"), "target/\n").expect("write ignore file");
        for file_number in 0..100 {
            let content = if file_number == 0 {
                "a synthetic needle used to measure first output\n"
            } else {
                "ordinary source content without the requested literal\n"
            };
            fs::write(root.join(format!("file-{file_number:03}.txt")), content)
                .expect("write fixture");
        }
        repos.push(RepoRecord {
            id: format!("fixture/repo-{repo_number:02}"),
            canonical_url: format!("fixture/repo-{repo_number:02}"),
            clone_url: format!("fixture/repo-{repo_number:02}"),
            local_path: root,
            default_branch: Some("main".into()),
            active: true,
            status: RepoStatus::Ready,
            sources: BTreeSet::from(["fixture".into()]),
            tags: BTreeSet::new(),
            last_error: None,
        });
    }

    let no_output: Arc<dyn Fn(SearchEvent) + Send + Sync> = Arc::new(drop);
    let no_match = SearchOptions {
        pattern: "literal-that-is-not-present".into(),
        fixed_strings: true,
        ..SearchOptions::default()
    };
    search(&repos, &no_match, &no_output)?;
    let mut timings = Vec::with_capacity(10);
    for _ in 0..10 {
        let started = Instant::now();
        search(&repos, &no_match, &no_output)?;
        timings.push(started.elapsed());
    }
    timings.sort_unstable();

    let started = Instant::now();
    let first_output = Arc::new(Mutex::new(None::<Duration>));
    let captured = Arc::clone(&first_output);
    let output: Arc<dyn Fn(SearchEvent) + Send + Sync> = Arc::new(move |event| {
        if matches!(event, SearchEvent::Match(_)) {
            let mut first = captured.lock().expect("first-output lock");
            first.get_or_insert_with(|| started.elapsed());
        }
    });
    search(
        &repos,
        &SearchOptions {
            pattern: "needle".into(),
            fixed_strings: true,
            ..SearchOptions::default()
        },
        &output,
    )?;

    println!(
        "60 repos / 6,000 files: median warm no-match {:?}; first output {:?}",
        timings[timings.len() / 2],
        first_output
            .lock()
            .expect("first-output lock")
            .expect("a match")
    );
    Ok(())
}
