mod bitbucket;
mod github;
mod manifest;

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};

use crate::config::{CONFIG_FILE_NAME, RepositoryConfig, SourceConfig, validate_repo_name};
use crate::error::{Error, Result};
use crate::model::RepoSpec;

pub(crate) async fn discover(config: &SourceConfig) -> Result<Vec<RepoSpec>> {
    match config {
        SourceConfig::GitHub(config) => github::discover(config).await,
        SourceConfig::BitbucketServer(config) => {
            bitbucket::BitbucketSource::new(config)?.discover().await
        }
        SourceConfig::Manifest(config) => manifest::discover(config),
    }
}

pub(crate) fn discover_repositories(repositories: &[RepositoryConfig]) -> Result<Vec<RepoSpec>> {
    repository_specs(None, repositories, &[], CONFIG_FILE_NAME)
}

fn repository_specs(
    source: Option<&str>,
    repositories: &[RepositoryConfig],
    default_tags: &[String],
    context: &str,
) -> Result<Vec<RepoSpec>> {
    let mut ids = HashSet::new();
    let mut specs = Vec::with_capacity(repositories.len());
    for repository in repositories {
        validate_repo_name(&repository.id)?;
        if !ids.insert(&repository.id) {
            return Err(Error::Config(format!(
                "duplicate repository id {:?} in {context}",
                repository.id
            )));
        }
        let id = source.map_or_else(
            || repository.id.clone(),
            |source| format!("{source}/{}", repository.id),
        );
        specs.push(RepoSpec {
            id,
            canonical_url: canonicalize_remote(&repository.url)?,
            clone_url: repository.url.clone(),
            default_branch: repository.default_branch.clone(),
            tags: tags(default_tags, &repository.tags),
        });
    }
    Ok(specs)
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

pub(crate) fn build_globs(values: &[String]) -> Result<GlobSet> {
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

fn authenticated_headers(token: &str, accept: &'static str, provider: &str) -> Result<HeaderMap> {
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
    use super::{AUTHORIZATION, authenticated_headers, canonicalize_remote};

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

    #[test]
    fn builds_a_sensitive_authorization_header_without_leaking_invalid_tokens() {
        let headers = authenticated_headers("example-token", "application/json", "test").unwrap();
        let authorization = headers.get(AUTHORIZATION).unwrap();
        assert_eq!(authorization, "Bearer example-token");
        assert!(authorization.is_sensitive());

        let error = authenticated_headers("secret\nsensitive-fragment", "application/json", "test")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret"));
        assert!(!error.contains("sensitive-fragment"));
    }
}
