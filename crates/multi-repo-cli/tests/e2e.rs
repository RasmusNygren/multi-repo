use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

#[test]
fn manifest_sync_list_and_working_tree_search() {
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
    fs::write(seed.join("README.md"), "needle from remote\n").unwrap();
    fs::write(seed.join(".gitignore"), "ignored/\n").unwrap();
    git(&seed, ["add", "."]);
    git(&seed, ["commit", "-m", "initial"]);
    git(&seed, ["remote", "add", "origin", text(&remote)]);
    git(&seed, ["push", "-u", "origin", "main"]);

    let manifest = temp.path().join("repos.toml");
    fs::write(
        &manifest,
        format!(
            "version = 1\n\n[[repos]]\nid = \"acme/repo\"\nurl = {:?}\ndefault_branch = \"main\"\ntags = [\"test\"]\n",
            text(&remote)
        ),
    )
    .unwrap();
    let config = temp.path().join("config.toml");
    fs::write(
        &config,
        format!(
            "version = 1\nroot = {:?}\n\n[[sources]]\nname = \"manual\"\nkind = \"manifest\"\npath = {:?}\n",
            text(&temp.path().join("managed")),
            text(&manifest)
        ),
    )
    .unwrap();

    let sync = command(&config, ["sync"]);
    assert_success(&sync);
    assert!(String::from_utf8_lossy(&sync.stdout).contains("manual/acme/repo: cloned"));

    let list = command(&config, ["list", "--tag", "test"]);
    assert_success(&list);
    assert!(String::from_utf8_lossy(&list.stdout).contains("manual/acme/repo"));

    let checkout = temp.path().join("managed/repos/manual/acme/repo");
    fs::write(checkout.join("local.txt"), "needle from untracked\n").unwrap();
    fs::create_dir(checkout.join("ignored")).unwrap();
    fs::write(checkout.join("ignored/no.txt"), "needle ignored\n").unwrap();
    let grep = command(&config, ["grep", "-F", "needle", "--sort-path"]);
    assert_success(&grep);
    let stdout = String::from_utf8_lossy(&grep.stdout);
    assert!(stdout.contains("manual/acme/repo:README.md:1:1:needle from remote"));
    assert!(stdout.contains("manual/acme/repo:local.txt:1:1:needle from untracked"));
    assert!(!stdout.contains("ignored/no.txt"));

    let repos = command(&config, ["grep", "needle", "--repos-with-matches"]);
    assert_success(&repos);
    assert_eq!(
        String::from_utf8_lossy(&repos.stdout).trim(),
        "manual/acme/repo"
    );

    let json_output = command(&config, ["grep", "-F", "needle", "--json"]);
    assert_success(&json_output);
    let events = String::from_utf8(json_output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event["type"] == "match"));
    assert!(
        events
            .iter()
            .all(|event| event["repo"] == "manual/acme/repo")
    );

    let no_match = command(&config, ["grep", "absent-literal"]);
    assert_eq!(no_match.status.code(), Some(1));
    assert!(no_match.stdout.is_empty());
}

fn command<I, S>(config: &Path, args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_multi-repo"))
        .arg("--config")
        .arg(config)
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
