mod bitbucket;
mod github;
mod manifest;

use std::collections::BTreeSet;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::config::SourceConfig;
use crate::error::{Error, Result};
use crate::model::RepoSpec;

pub(crate) enum Provider {
    GitHub(github::GitHubSource),
    Bitbucket(bitbucket::BitbucketSource),
    Manifest(manifest::ManifestSource),
}

impl Provider {
    pub(crate) async fn discover(&self) -> Result<Vec<RepoSpec>> {
        match self {
            Self::GitHub(source) => source.discover().await,
            Self::Bitbucket(source) => source.discover().await,
            Self::Manifest(source) => source.discover(),
        }
    }
}

pub(crate) fn from_config(config: &SourceConfig) -> Result<Provider> {
    match config {
        SourceConfig::GitHub(config) => Ok(Provider::GitHub(github::GitHubSource::new(config)?)),
        SourceConfig::BitbucketServer(config) => Ok(Provider::Bitbucket(
            bitbucket::BitbucketSource::new(config)?,
        )),
        SourceConfig::Manifest(config) => {
            Ok(Provider::Manifest(manifest::ManifestSource::new(config)))
        }
    }
}

pub(crate) struct Filters {
    include: GlobSet,
    exclude: GlobSet,
    include_all: bool,
}

impl Filters {
    pub(crate) fn new(include: &[String], exclude: &[String]) -> Result<Self> {
        Ok(Self {
            include: build_globs(include)?,
            exclude: build_globs(exclude)?,
            include_all: include.is_empty(),
        })
    }

    pub(crate) fn matches(&self, value: &str) -> bool {
        (self.include_all || self.include.is_match(value)) && !self.exclude.is_match(value)
    }
}

fn build_globs(values: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for value in values {
        builder.add(
            Glob::new(value)
                .map_err(|error| Error::Config(format!("invalid glob {value:?}: {error}")))?,
        );
    }
    builder
        .build()
        .map_err(|error| Error::Config(format!("invalid glob set: {error}")))
}

pub(crate) fn tags(defaults: &[String], specific: &[String]) -> BTreeSet<String> {
    defaults
        .iter()
        .chain(specific)
        .filter(|tag| !tag.is_empty())
        .cloned()
        .collect()
}

pub(crate) fn token_from_env(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| {
        Error::Config(format!(
            "environment variable {name:?} is required for repository discovery"
        ))
    })
}

pub(crate) fn canonicalize_remote(remote: &str) -> Result<String> {
    let trimmed = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    if trimmed.contains("://") {
        let url = url::Url::parse(trimmed)?;
        let host = url
            .host_str()
            .ok_or_else(|| Error::Config(format!("clone URL has no host: {remote:?}")))?
            .to_ascii_lowercase();
        let path = url.path().trim_matches('/').trim_end_matches(".git");
        return Ok(format!("{host}/{path}"));
    }
    if let Some((host, path)) = trimmed.rsplit_once(':') {
        let host = host.rsplit('@').next().unwrap_or(host).to_ascii_lowercase();
        return Ok(format!("{host}/{}", path.trim_matches('/')));
    }
    let path = Path::new(trimmed);
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(format!("file/{}", absolute.to_string_lossy()))
}
