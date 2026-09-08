use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

#[test]
fn fetch_all_branches_supports_new_and_existing_clones() {
    for all_branches_on_clone in [false, true] {
        for default_branch in ["", "default_branch = \"main\"\n"] {
            assert_branch_fetch_behavior(all_branches_on_clone, default_branch);
        }
    }
}

fn assert_branch_fetch_behavior(all_branches_on_clone: bool, default_branch: &str) {
    let temp = TempDir::new().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    git(
        temp.path(),
        ["init", "--bare", "--initial-branch=main", text(&remote)],
    );
    git(temp.path(), ["init", "--initial-branch=main", text(&seed)]);
    git(&seed, ["config", "user.name", "Test"]);
    git(&seed, ["config", "user.email", "test@example.com"]);
    fs::write(seed.join("file.txt"), "main\n").unwrap();
    git(&seed, ["add", "."]);
    git(&seed, ["commit", "-m", "initial"]);
    git(&seed, ["branch", "feature/topic"]);
    git(&seed, ["remote", "add", "origin", text(&remote)]);
    git(&seed, ["push", "origin", "main", "feature/topic"]);

    let config = temp.path().join(".multi-repo.toml");
    let repositories = format!(
        "\n[[repo]]\nid = \"repo\"\nurl = {:?}\n{default_branch}",
        text(&remote)
    );
    let setting = if all_branches_on_clone {
        "fetch_all_branches = true\n"
    } else {
        ""
    };
    fs::write(&config, format!("version = 1\n{setting}{repositories}")).unwrap();
    assert_success(&command(temp.path(), ["sync"]));
    let checkout = temp.path().join("repos/repo");
    let main_head = git_revision(&seed, "main");
    assert_eq!(git_revision(&checkout, "HEAD"), main_head);
    assert_eq!(
        git_revision(&checkout, "origin/feature/topic").is_some(),
        all_branches_on_clone
    );

    git(&seed, ["checkout", "feature/topic"]);
    fs::write(seed.join("file.txt"), "feature\n").unwrap();
    git(&seed, ["commit", "-am", "feature update"]);
    git(&seed, ["push", "origin", "feature/topic"]);
    let feature_head = git_revision(&seed, "HEAD");
    assert_success(&command(temp.path(), ["sync"]));
    assert_eq!(
        git_revision(&checkout, "origin/feature/topic"),
        if all_branches_on_clone {
            feature_head.clone()
        } else {
            None
        }
    );

    fs::write(
        &config,
        format!("version = 1\nfetch_all_branches = true\n{repositories}"),
    )
    .unwrap();
    assert_success(&command(temp.path(), ["sync"]));
    assert_eq!(
        git_revision(&checkout, "origin/feature/topic"),
        feature_head
    );
    assert_eq!(git_revision(&checkout, "HEAD"), main_head);
    assert_eq!(
        fs::read_to_string(checkout.join("file.txt")).unwrap(),
        "main\n"
    );

    // Fetching updates remote refs without moving a checked-out feature branch or its files.
    git(
        &checkout,
        ["checkout", "-b", "local-feature", "origin/feature/topic"],
    );
    fs::write(checkout.join("file.txt"), "local changes\n").unwrap();
    git(
        &seed,
        ["commit", "--allow-empty", "-m", "another feature update"],
    );
    git(&seed, ["push", "origin", "feature/topic"]);
    assert_success(&command(temp.path(), ["sync"]));
    assert_eq!(
        git_revision(&checkout, "origin/feature/topic"),
        git_revision(&seed, "HEAD")
    );
    assert_eq!(git_revision(&checkout, "HEAD"), feature_head);
    assert_eq!(
        fs::read_to_string(checkout.join("file.txt")).unwrap(),
        "local changes\n"
    );

    git(&seed, ["push", "origin", "--delete", "feature/topic"]);
    assert_success(&command(temp.path(), ["sync"]));
    assert!(git_revision(&checkout, "origin/feature/topic").is_none());
    assert_eq!(
        git_revision(&checkout, "refs/heads/local-feature"),
        feature_head
    );
}

fn git_revision(path: &Path, reference: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--verify", "--quiet", reference])
        .output()
        .unwrap();
    if output.status.code() == Some(1) {
        return None;
    }
    assert_success(&output);
    Some(String::from_utf8(output.stdout).unwrap().trim().to_owned())
}

#[test]
fn inline_repository_sync_list_and_working_tree_search() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().join("workspace");
    let nested = workspace.join("nested/directory");
    fs::create_dir_all(&nested).unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    git(
        temp.path(),
        ["init", "--bare", "--initial-branch=main", text(&remote)],
    );
    git(temp.path(), ["init", "--initial-branch=main", text(&seed)]);
    git(&seed, ["config", "user.name", "Test"]);
    git(&seed, ["config", "user.email", "test@example.com"]);
    fs::write(seed.join("README.md"), "needle from remote\n").unwrap();
    fs::write(seed.join(".gitignore"), "ignored/\n").unwrap();
    git(&seed, ["add", "."]);
    git(&seed, ["commit", "-m", "initial"]);
    git(&seed, ["remote", "add", "origin", text(&remote)]);
    git(&seed, ["push", "-u", "origin", "main"]);

    let config = workspace.join(".multi-repo.toml");
    fs::write(
        &config,
        format!(
            "version = 1\n\n[[repo]]\nid = \"acme/repo\"\nurl = {:?}\ndefault_branch = \"main\"\ntags = [\"test\"]\n",
            text(&remote)
        ),
    )
    .unwrap();

    let dry_run = command(&nested, ["sync", "--dry-run"]);
    assert_success(&dry_run);
    let dry_run_stdout = String::from_utf8_lossy(&dry_run.stdout);
    assert!(dry_run_stdout.contains("source workspace: 1 repository"));
    assert!(dry_run_stdout.contains("  + acme/repo"));
    assert!(!workspace.join(".multi-repo").exists());

    let sync = command(&nested, ["sync"]);
    assert_success(&sync);
    assert!(String::from_utf8_lossy(&sync.stdout).contains("acme/repo: cloned"));

    let unchanged = command(&nested, ["sync", "--dry-run"]);
    assert_success(&unchanged);
    let unchanged_stdout = String::from_utf8_lossy(&unchanged.stdout);
    assert!(unchanged_stdout.contains("repository changes: none"));
    assert!(!unchanged_stdout.contains("unchanged"));

    let list = command(&nested, ["list", "--tag", "test", "--source", "workspace"]);
    assert_success(&list);
    assert!(String::from_utf8_lossy(&list.stdout).contains("acme/repo"));

    let checkout = workspace.join("repos/acme/repo");
    fs::write(checkout.join("local.txt"), "needle from untracked\n").unwrap();
    fs::create_dir(checkout.join("ignored")).unwrap();
    fs::write(checkout.join("ignored/no.txt"), "needle ignored\n").unwrap();
    let grep = command(&nested, ["grep", "-F", "needle", "--sort-path"]);
    assert_success(&grep);
    let stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(stdout.contains("acme/repo:README.md:1:1:needle from remote"));
    assert!(stdout.contains("acme/repo:local.txt:1:1:needle from untracked"));
    assert!(!stdout.contains("ignored/no.txt"));

    let colored = command(
        &nested,
        ["grep", "-F", "needle", "--sort-path", "--color", "always"],
    );
    assert_success(&colored);
    let colored_stdout = String::from_utf8_lossy(&colored.stdout);
    assert!(colored_stdout.contains("\x1b[1;31mneedle\x1b[0m from remote"));
    assert!(colored_stdout.contains("\x1b[1;31mneedle\x1b[0m from untracked"));

    let repos = command(&nested, ["grep", "needle", "--repos-with-matches"]);
    assert_success(&repos);
    assert_eq!(String::from_utf8_lossy(&repos.stdout).trim(), "acme/repo");

    let json_output = command(
        &nested,
        ["grep", "-F", "needle", "--json", "--color", "always"],
    );
    assert_success(&json_output);
    let events = String::from_utf8(json_output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event["type"] == "match"));
    assert!(events.iter().all(|event| event["repo"] == "acme/repo"));

    let no_match = command(&nested, ["grep", "absent-literal"]);
    assert_eq!(no_match.status.code(), Some(1));
    assert!(no_match.stdout.is_empty());

    assert_prune_behavior(&workspace, &nested, &config, &checkout);
}

fn assert_prune_behavior(workspace: &Path, nested: &Path, config: &Path, checkout: &Path) {
    let orphan = workspace.join("repos/github/IOOPM-UU");
    fs::create_dir_all(&orphan).unwrap();
    assert_success(&command(nested, ["prune"]));
    assert!(!orphan.exists());
    assert!(checkout.exists());

    fs::write(config, "version = 1\n").unwrap();
    let dry_sync = command(nested, ["sync", "--dry-run"]);
    assert_success(&dry_sync);
    assert!(String::from_utf8_lossy(&dry_sync.stdout).contains("  - acme/repo"));
    assert!(String::from_utf8_lossy(&command(nested, ["list"]).stdout).contains("acme/repo"));
    assert_success(&command(nested, ["sync"]));
    assert!(command(nested, ["list"]).stdout.is_empty());
    let inactive = command(nested, ["list", "--all"]);
    assert_success(&inactive);
    assert!(String::from_utf8_lossy(&inactive.stdout).contains("acme/repo (inactive)"));

    let dirty_prune = command(nested, ["prune"]);
    assert_success(&dirty_prune);
    assert!(
        String::from_utf8_lossy(&dirty_prune.stdout).contains("working tree has local changes")
    );
    assert!(checkout.exists());

    fs::remove_file(checkout.join("local.txt")).unwrap();
    fs::write(checkout.join("README.md"), "local committed work\n").unwrap();
    git(checkout, ["config", "user.name", "Test"]);
    git(checkout, ["config", "user.email", "test@example.com"]);
    git(checkout, ["add", "README.md"]);
    git(checkout, ["commit", "-m", "local work"]);
    let local_state_prune = command(nested, ["prune"]);
    assert_success(&local_state_prune);
    assert!(String::from_utf8_lossy(&local_state_prune.stdout).contains("local commits"));
    assert!(checkout.exists());

    git(checkout, ["reset", "--hard", "origin/main"]);
    let dry_prune = command(nested, ["prune", "--dry-run"]);
    assert_success(&dry_prune);
    assert!(String::from_utf8_lossy(&dry_prune.stdout).contains("acme/repo: would delete"));
    assert!(checkout.exists());

    let prune = command(nested, ["prune"]);
    assert_success(&prune);
    assert!(String::from_utf8_lossy(&prune.stdout).contains("acme/repo: deleted"));
    assert!(!checkout.exists());
    assert!(!workspace.join("repos").exists());
    assert!(command(nested, ["list", "--all"]).stdout.is_empty());
}

fn command<I, S>(directory: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_multi-repo"))
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap()
}

fn git<I, S>(path: &Path, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert_success(&output);
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}
