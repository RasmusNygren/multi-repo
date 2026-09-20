use std::collections::BTreeSet;
use std::time::Duration;

use reqwest::header::LINK;
use serde::Deserialize;

use super::{Filters, authenticated_headers, canonicalize_remote};
use crate::config::GitHubConfig;
use crate::error::Result;
use crate::model::{CloneProtocol, RepoSpec};

pub(super) async fn discover(config: &GitHubConfig) -> Result<Vec<RepoSpec>> {
    let token = config.access_token()?;
    let headers = authenticated_headers(&token, "application/vnd.github+json", "GitHub")?;
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;
    let filters = Filters::new(&config.include, &config.exclude)?;
    let tags: BTreeSet<String> = config.tags.iter().cloned().collect();
    let mut next = Some(format!(
        "{}/user/repos?per_page=100&affiliation=owner,collaborator,organization_member",
        config.api_url.trim_end_matches('/')
    ));
    let mut discovered = Vec::new();
    while let Some(url) = next.take() {
        let response = client.get(&url).send().await?.error_for_status()?;
        next = response
            .headers()
            .get(LINK)
            .and_then(|value| value.to_str().ok())
            .and_then(next_link);
        let repos: Vec<GitHubRepo> = response.json().await?;
        for repo in repos {
            if (!config.include_forks && repo.fork)
                || (!config.include_archived && repo.archived)
                || (!config.include_private && repo.private)
                || !filters.matches(&repo.full_name)
            {
                continue;
            }
            let clone_url = match config.clone_protocol {
                CloneProtocol::Ssh => repo.ssh_url,
                CloneProtocol::Https => repo.clone_url,
            };
            discovered.push(RepoSpec {
                id: format!("{}/{}", config.name, repo.full_name),
                canonical_url: canonicalize_remote(&clone_url)?,
                clone_url,
                default_branch: Some(repo.default_branch),
                tags: tags.clone(),
            });
        }
    }
    Ok(discovered)
}

fn next_link(value: &str) -> Option<String> {
    value.split(',').find_map(|part| {
        let (url, attributes) = part.trim().split_once(';')?;
        if attributes
            .split(';')
            .any(|attribute| attribute.trim() == "rel=\"next\"")
        {
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
