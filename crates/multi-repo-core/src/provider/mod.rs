mod bitbucket;
mod github;
mod manifest;

use std::collections::BTreeSet;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};

use crate::config::SourceConfig;
use crate::error::{Error, Result};
use crate::model::RepoSpec;

pub(crate) async fn discover(config: &SourceConfig) -> Result<Vec<RepoSpec>> {
    match config {
        SourceConfig::GitHub(config) => github::GitHubSource::new(config)?.discover().await,
        SourceConfig::BitbucketServer(config) => {
            bitbucket::BitbucketSource::new(config)?.discover().await
        }
        SourceConfig::Manifest(config) => manifest::ManifestSource::new(config).discover(),
    }
}

struct Filters {
    include: GlobSet,
    exclude: GlobSet,
    include_all: bool,
}

impl Filters {
    fn new(include: &[String], exclude: &[String]) -> Result<Self> {
        Ok(Self {
            include: build_globs(include)?,
            exclude: build_globs(exclude)?,
            include_all: include.is_empty(),
        })
    }

    fn matches(&self, value: &str) -> bool {
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

fn tags(defaults: &[String], specific: &[String]) -> BTreeSet<String> {
    defaults
        .iter()
        .chain(specific)
        .filter(|tag| !tag.is_empty())
        .cloned()
        .collect()
}

fn authenticated_headers(
    token_env: &str,
    accept: &'static str,
    provider: &str,
) -> Result<HeaderMap> {
    let token = std::env::var(token_env).map_err(|_| {
        Error::Config(format!(
            "environment variable {token_env:?} is required for repository discovery"
        ))
    })?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|error| Error::Config(format!("invalid {provider} token: {error}")))?;
    authorization.set_sensitive(true);

    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("multi-repo/", env!("CARGO_PKG_VERSION"))),
    );
    headers.insert(ACCEPT, HeaderValue::from_static(accept));
    headers.insert(AUTHORIZATION, authorization);
    Ok(headers)
}

pub(crate) fn canonicalize_remote(remote: &str) -> Result<String> {
    let trimmed = remote.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        let url = url::Url::parse(trimmed)?;
        let host = url
            .host_str()
            .ok_or_else(|| Error::Config(format!("clone URL has no host: {remote:?}")))?
            .to_ascii_lowercase();
        let authority = url
            .port()
            .map_or_else(|| host.clone(), |port| format!("{host}:{port}"));
        let path = strip_git_suffix(url.path().trim_matches('/'));
        return Ok(format!("{authority}/{path}"));
    }
    if let Some((host, path)) = trimmed.rsplit_once(':') {
        let host = host.rsplit('@').next().unwrap_or(host).to_ascii_lowercase();
        return Ok(format!(
            "{host}/{}",
            strip_git_suffix(path.trim_matches('/'))
        ));
    }
    let path = Path::new(strip_git_suffix(trimmed));
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(format!("file/{}", absolute.to_string_lossy()))
}

fn strip_git_suffix(value: &str) -> &str {
    value.strip_suffix(".git").unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::canonicalize_remote;

    #[test]
    fn canonicalizes_common_remote_forms_without_losing_ports() {
        assert_eq!(
            canonicalize_remote("git@GitHub.com:acme/widget.git").unwrap(),
            "github.com/acme/widget"
        );
        assert_eq!(
            canonicalize_remote("https://example.com:8443/acme/widget.git").unwrap(),
            "example.com:8443/acme/widget"
        );
    }
}
