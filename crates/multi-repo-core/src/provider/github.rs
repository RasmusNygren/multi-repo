use std::collections::BTreeSet;
use std::time::Duration;

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, LINK, USER_AGENT};
use serde::Deserialize;

use super::{Filters, canonicalize_remote, token_from_env};
use crate::config::GitHubConfig;
use crate::error::{Error, Result};
use crate::model::{CloneProtocol, RepoSpec};

pub(crate) struct GitHubSource {
    name: String,
    api_url: String,
    client: reqwest::Client,
    protocol: CloneProtocol,
    filters: Filters,
    include_forks: bool,
    include_archived: bool,
    include_private: bool,
    tags: BTreeSet<String>,
}

impl GitHubSource {
    pub(crate) fn new(config: &GitHubConfig) -> Result<Self> {
        let token = token_from_env(&config.token_env)?;
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("multi-repo/0.1"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|error| Error::Config(format!("invalid GitHub token: {error}")))?,
        );
        Ok(Self {
            name: config.name.clone(),
            api_url: config.api_url.trim_end_matches('/').to_owned(),
            client: reqwest::Client::builder()
                .default_headers(headers)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()?,
            protocol: config.clone_protocol,
            filters: Filters::new(&config.include, &config.exclude)?,
            include_forks: config.include_forks,
            include_archived: config.include_archived,
            include_private: config.include_private,
            tags: config.tags.iter().cloned().collect(),
        })
    }

    pub(crate) async fn discover(&self) -> Result<Vec<RepoSpec>> {
        let mut next = Some(format!(
            "{}/user/repos?per_page=100&affiliation=owner,collaborator,organization_member",
            self.api_url
        ));
        let mut discovered = Vec::new();
        while let Some(url) = next.take() {
            let response = self.client.get(&url).send().await?.error_for_status()?;
            next = response
                .headers()
                .get(LINK)
                .and_then(|value| value.to_str().ok())
                .and_then(next_link);
            let repos: Vec<GitHubRepo> = response.json().await?;
            for repo in repos {
                if (!self.include_forks && repo.fork)
                    || (!self.include_archived && repo.archived)
                    || (!self.include_private && repo.private)
                    || !self.filters.matches(&repo.full_name)
                {
                    continue;
                }
                let clone_url = match self.protocol {
                    CloneProtocol::Ssh => repo.ssh_url,
                    CloneProtocol::Https => repo.clone_url,
                };
                discovered.push(RepoSpec {
                    id: format!("{}/{}", self.name, repo.full_name),
                    source: self.name.clone(),
                    canonical_url: canonicalize_remote(&clone_url)?,
                    clone_url,
                    default_branch: Some(repo.default_branch),
                    tags: self.tags.clone(),
                });
            }
        }
        Ok(discovered)
    }
}

fn next_link(value: &str) -> Option<String> {
    value.split(',').find_map(|part| {
        let (url, attributes) = part.trim().split_once(';')?;
        if attributes.trim() == "rel=\"next\"" {
            Some(url.trim().trim_matches(['<', '>']).to_owned())
        } else {
            None
        }
    })
}

#[derive(Debug, Deserialize)]
struct GitHubRepo {
    full_name: String,
    ssh_url: String,
    clone_url: String,
    default_branch: String,
    fork: bool,
    archived: bool,
    private: bool,
}

#[cfg(test)]
mod tests {
    use super::next_link;

    #[test]
    fn parses_next_link() {
        let value = "<https://api.github.test/repos?page=2>; rel=\"next\", <x>; rel=\"last\"";
        assert_eq!(
            next_link(value).as_deref(),
            Some("https://api.github.test/repos?page=2")
        );
    }
}
