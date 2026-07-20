use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::{RepoSource, canonicalize_remote, tags};
use crate::config::{ManifestConfig, validate_repo_name};
use crate::error::{Error, Result};
use crate::model::RepoSpec;

pub(crate) struct ManifestSource {
    name: String,
    path: PathBuf,
    default_tags: Vec<String>,
}

impl ManifestSource {
    pub(crate) fn new(config: &ManifestConfig) -> Self {
        Self {
            name: config.name.clone(),
            path: config.path.clone(),
            default_tags: config.tags.clone(),
        }
    }
}

#[async_trait]
impl RepoSource for ManifestSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn discover(&self) -> Result<Vec<RepoSpec>> {
        let contents = std::fs::read_to_string(&self.path).map_err(|source| Error::Read {
            path: self.path.clone(),
            source,
        })?;
        let manifest: Manifest = toml::from_str(&contents)?;
        if manifest.version != 1 {
            return Err(Error::Config(format!(
                "unsupported manifest version {}; expected 1",
                manifest.version
            )));
        }
        let mut ids = HashSet::new();
        let mut repos = Vec::with_capacity(manifest.repos.len());
        for repo in manifest.repos {
            validate_repo_name(&repo.id)?;
            if !ids.insert(repo.id.clone()) {
                return Err(Error::Config(format!(
                    "duplicate repository id {:?} in {}",
                    repo.id,
                    self.path.display()
                )));
            }
            repos.push(RepoSpec {
                id: format!("{}/{}", self.name, repo.id),
                source: self.name.clone(),
                canonical_url: canonicalize_remote(&repo.url)?,
                clone_url: repo.url,
                default_branch: repo.default_branch,
                tags: tags(&self.default_tags, &repo.tags),
            });
        }
        Ok(repos)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    #[serde(default)]
    repos: Vec<ManifestRepo>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRepo {
    id: String,
    url: String,
    default_branch: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}
