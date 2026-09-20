use serde::Deserialize;

use super::repository_specs;
use crate::config::{ManifestConfig, RepositoryConfig, resolve_repository_url};
use crate::error::{Error, Result};
use crate::model::RepoSpec;

pub(super) fn discover(config: &ManifestConfig) -> Result<Vec<RepoSpec>> {
    let contents = std::fs::read_to_string(&config.path).map_err(|source| Error::Read {
        path: config.path.clone(),
        source,
    })?;
    let mut manifest: Manifest = toml::from_str(&contents)?;
    let base = config
        .path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    for repository in &mut manifest.repos {
        repository.url = resolve_repository_url(base, &repository.url)?;
    }
    let context = config.path.display().to_string();
    repository_specs(Some(&config.name), &manifest.repos, &config.tags, &context)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    #[serde(default, rename = "repo")]
    repos: Vec<RepositoryConfig>,
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn discovers_external_manifest_relative_to_its_file() {
        let temp = TempDir::new().unwrap();
        let directory = temp.path().join("catalog");
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("repos.toml");
        std::fs::write(
            &path,
            "[[repo]]\nid = \"acme/widget\"\nurl = \"../widget.git\"\ntags = [\"specific\"]\n",
        )
        .unwrap();
        let config = ManifestConfig {
            name: "generated".into(),
            fetch_all_branches: false,
            path,
            tags: vec!["default".into()],
        };

        let repositories = discover(&config).unwrap();

        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].id, "generated/acme/widget");
        assert_eq!(
            repositories[0].clone_url,
            directory.join("../widget.git").to_string_lossy()
        );
        assert_eq!(
            repositories[0].tags,
            ["default".to_owned(), "specific".to_owned()].into()
        );
    }

    #[test]
    fn rejects_obsolete_version_setting() {
        assert!(toml::from_str::<Manifest>("version = 1\n").is_err());
    }
}
