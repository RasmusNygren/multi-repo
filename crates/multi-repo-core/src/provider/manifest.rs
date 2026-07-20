use std::path::PathBuf;

use serde::Deserialize;

use super::repository_specs;
use crate::config::{ManifestConfig, RepositoryConfig, resolve_repository_url};
use crate::error::{Error, Result};
use crate::model::RepoSpec;

pub(super) struct ManifestSource {
    name: String,
    path: PathBuf,
    default_tags: Vec<String>,
}

impl ManifestSource {
    pub(super) fn new(config: &ManifestConfig) -> Self {
        Self {
            name: config.name.clone(),
            path: config.path.clone(),
            default_tags: config.tags.clone(),
        }
    }

    pub(super) fn discover(&self) -> Result<Vec<RepoSpec>> {
        let contents = std::fs::read_to_string(&self.path).map_err(|source| Error::Read {
            path: self.path.clone(),
            source,
        })?;
        let mut manifest: Manifest = toml::from_str(&contents)?;
        if manifest.version != 1 {
            return Err(Error::Config(format!(
                "unsupported manifest version {}; expected 1",
                manifest.version
            )));
        }
        let base = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        for repository in &mut manifest.repos {
            repository.url = resolve_repository_url(base, &repository.url)?;
        }
        let context = self.path.display().to_string();
        repository_specs(
            Some(&self.name),
            &manifest.repos,
            &self.default_tags,
            &context,
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
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
            "version = 1\n\n[[repo]]\nid = \"acme/widget\"\nurl = \"../widget.git\"\ntags = [\"specific\"]\n",
        )
        .unwrap();
        let source = ManifestSource::new(&ManifestConfig {
            name: "generated".into(),
            path,
            tags: vec!["default".into()],
        });

        let repositories = source.discover().unwrap();

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
}
