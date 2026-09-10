use std::collections::BTreeSet;
use std::path::Path;

use crate::error::Result;
use crate::git::files_at_revision;

const GO_TAG: &str = "language:go";
const PYTHON_TAG: &str = "language:python";
const RUST_TAG: &str = "language:rust";

pub(crate) fn detect(path: &Path, default_branch: &str) -> Result<BTreeSet<String>> {
    let revision = format!("refs/remotes/origin/{default_branch}");
    Ok(detect_paths(&files_at_revision(path, &revision)?))
}

fn detect_paths(paths: &[String]) -> BTreeSet<String> {
    let mut go_marker = false;
    let mut go_source = false;
    let mut python_marker = false;
    let mut python_source = false;
    let mut rust_marker = false;
    let mut rust_source = false;

    for path in paths {
        if is_dependency_path(path) {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        match name {
            "go.mod" => go_marker = true,
            "pyproject.toml" | "setup.py" | "setup.cfg" | "Pipfile" | "requirements.txt" => {
                python_marker = true;
            }
            "Cargo.toml" => rust_marker = true,
            _ => {}
        }
        match Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("go") => go_source = true,
            Some("py") => python_source = true,
            Some("rs") => rust_source = true,
            _ => {}
        }
    }

    let mut tags = BTreeSet::new();
    if go_marker && go_source {
        tags.insert(GO_TAG.to_owned());
    }
    if python_marker && python_source {
        tags.insert(PYTHON_TAG.to_owned());
    }
    if rust_marker && rust_source {
        tags.insert(RUST_TAG.to_owned());
    }
    tags
}

fn is_dependency_path(path: &str) -> bool {
    path.split('/').any(|component| {
        matches!(
            component,
            ".venv" | "node_modules" | "target" | "vendor" | "venv"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_languages_from_markers_and_sources() {
        let paths = strings(&[
            "services/api/go.mod",
            "services/api/main.go",
            "tools/pyproject.toml",
            "tools/src/tool.py",
            "Cargo.toml",
            "src/main.rs",
        ]);

        assert_eq!(
            detect_paths(&paths),
            BTreeSet::from([GO_TAG.into(), PYTHON_TAG.into(), RUST_TAG.into()])
        );
    }

    #[test]
    fn requires_both_a_project_marker_and_source_file() {
        let paths = strings(&["go.mod", "script.py", "Cargo.toml"]);

        assert!(detect_paths(&paths).is_empty());
    }

    #[test]
    fn ignores_dependency_directories() {
        let paths = strings(&[
            "go.mod",
            "vendor/example/dependency.go",
            "pyproject.toml",
            ".venv/lib/dependency.py",
            "Cargo.toml",
            "target/generated.rs",
        ]);

        assert!(detect_paths(&paths).is_empty());
    }

    fn strings(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }
}
