use std::collections::BTreeSet;
use std::time::Duration;

use serde::Deserialize;

use super::{Filters, authenticated_headers, canonicalize_remote};
use crate::config::BitbucketServerConfig;
use crate::error::{Error, Result};
use crate::model::{CloneProtocol, RepoSpec};

pub(super) struct BitbucketSource<'a> {
    config: &'a BitbucketServerConfig,
    client: reqwest::Client,
    filters: Filters,
    tags: BTreeSet<String>,
}

impl<'a> BitbucketSource<'a> {
    pub(super) fn new(config: &'a BitbucketServerConfig) -> Result<Self> {
        let token = config.access_token()?;
        let headers = authenticated_headers(&token, "application/json", "Bitbucket")?;
        let mut client = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30));
        if let Some(path) = &config.ca_bundle {
            let pem = std::fs::read(path).map_err(|source| Error::Read {
                path: path.clone(),
                source,
            })?;
            client = client.add_root_certificate(reqwest::Certificate::from_pem(&pem)?);
        }
        Ok(Self {
            config,
            client: client.build()?,
            filters: Filters::new(&config.include, &config.exclude)?,
            tags: config.tags.iter().cloned().collect(),
        })
    }

    async fn discover_url(&self, url: String) -> Result<Vec<RepoSpec>> {
        let mut start = None;
        let mut discovered = Vec::new();
        loop {
            let separator = if url.contains('?') { '&' } else { '?' };
            let page_url = start.map_or_else(
                || url.clone(),
                |value| format!("{url}{separator}start={value}"),
            );
            let page: Page = self
                .client
                .get(page_url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            for repo in page.values {
                let name = format!("{}/{}", repo.project.key, repo.slug);
                if (!self.config.include_archived && repo.archived) || !self.filters.matches(&name)
                {
                    continue;
                }
                let requested = match self.config.clone_protocol {
                    CloneProtocol::Ssh => "ssh",
                    CloneProtocol::Https => "http",
                };
                let clone_url = repo
                    .links
                    .clone
                    .into_iter()
                    .find(|link| link.name == requested)
                    .ok_or_else(|| {
                        Error::Config(format!(
                            "Bitbucket repository {name} has no {requested} clone URL"
                        ))
                    })?
                    .href;
                discovered.push(RepoSpec {
                    id: format!("{}/{}", self.config.name, name),
                    canonical_url: canonicalize_remote(&clone_url)?,
                    clone_url,
                    default_branch: repo.default_branch.map(|branch| branch.display_id),
                    tags: self.tags.clone(),
                });
            }
            if page.last {
                break;
            }
            start = page.next_page_start;
            if start.is_none() {
                return Err(Error::Config(
                    "Bitbucket response is not the last page but has no nextPageStart".into(),
                ));
            }
        }
        Ok(discovered)
    }
    pub(super) async fn discover(&self) -> Result<Vec<RepoSpec>> {
        let mut discovered = Vec::new();
        if self.config.projects.is_empty() {
            discovered.extend(
                self.discover_url(format!(
                    "{}/rest/api/1.0/repos?limit=100&permission=REPO_READ",
                    self.config.base_url.trim_end_matches('/')
                ))
                .await?,
            );
        } else {
            for project in &self.config.projects {
                discovered.extend(
                    self.discover_url(format!(
                        "{}/rest/api/1.0/projects/{project}/repos?limit=100",
                        self.config.base_url.trim_end_matches('/')
                    ))
                    .await?,
                );
            }
        }
        Ok(discovered)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    values: Vec<BitbucketRepo>,
    #[serde(rename = "isLastPage")]
    last: bool,
    next_page_start: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitbucketRepo {
    slug: String,
    project: Project,
    #[serde(default)]
    archived: bool,
    default_branch: Option<Branch>,
    links: Links,
}

#[derive(Debug, Deserialize)]
struct Project {
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Branch {
    display_id: String,
}

#[derive(Debug, Deserialize)]
struct Links {
    #[serde(rename = "clone")]
    clone: Vec<CloneLink>,
}

#[derive(Debug, Deserialize)]
struct CloneLink {
    name: String,
    href: String,
}

#[cfg(test)]
mod tests {
    use super::Page;

    #[test]
    fn parses_bitbucket_819_repository_page() {
        let page: Page = serde_json::from_str(
            r#"{
                "values": [{
                    "slug": "widget",
                    "project": {"key": "PLATFORM"},
                    "archived": false,
                    "defaultBranch": {"displayId": "main"},
                    "links": {"clone": [
                        {"name": "ssh", "href": "ssh://git@stash/PLATFORM/widget.git"},
                        {"name": "http", "href": "https://stash/scm/platform/widget.git"}
                    ]}
                }],
                "isLastPage": false,
                "nextPageStart": 100
            }"#,
        )
        .unwrap();
        assert_eq!(page.values[0].project.key, "PLATFORM");
        assert_eq!(page.next_page_start, Some(100));
        assert!(!page.last);
    }
}
