use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::CloneProtocol;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub root: PathBuf,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SourceConfig {
    #[serde(rename = "github")]
    GitHub(GitHubConfig),
    #[serde(rename = "bitbucket-server")]
    BitbucketServer(BitbucketServerConfig),
    #[serde(rename = "manifest")]
    Manifest(ManifestConfig),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubConfig {
    pub name: String,
    #[serde(default = "default_github_api_url")]
    pub api_url: String,
    pub token_env: String,
    #[serde(default)]
    pub clone_protocol: CloneProtocol,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub include_forks: bool,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default = "default_true")]
    pub include_private: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BitbucketServerConfig {
    pub name: String,
    pub base_url: String,
    pub token_env: String,
    #[serde(default)]
    pub clone_protocol: CloneProtocol,
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    pub ca_bundle: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestConfig {
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_github_api_url() -> String {
    "https://api.github.com".to_owned()
}

const fn default_true() -> bool {
    true
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => default_config_path()?,
        };
        let contents = std::fs::read_to_string(&path).map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        let mut config: Self = toml::from_str(&contents)?;
        if config.version != CONFIG_VERSION {
            return Err(Error::Config(format!(
                "unsupported config version {}; expected {CONFIG_VERSION}",
                config.version
            )));
        }

        let base = path.parent().unwrap_or_else(|| Path::new("."));
        config.root = resolve_path(base, &config.root)?;
        let mut names = HashSet::new();
        for source in &mut config.sources {
            let name = source.name();
            validate_component(name, "source name")?;
            if !names.insert(name.to_owned()) {
                return Err(Error::Config(format!("duplicate source name {name:?}")));
            }
            match source {
                SourceConfig::Manifest(manifest) => {
                    manifest.path = resolve_path(base, &manifest.path)?;
                }
                SourceConfig::BitbucketServer(bitbucket) => {
                    if let Some(bundle) = &bitbucket.ca_bundle {
                        bitbucket.ca_bundle = Some(resolve_path(base, bundle)?);
                    }
                }
                SourceConfig::GitHub(_) => {}
            }
        }
        Ok((config, path))
    }

    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.root.join(".multi-repo")
    }

    #[must_use]
    pub fn repos_dir(&self) -> PathBuf {
        self.root.join("repos")
    }
}

impl SourceConfig {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::GitHub(config) => &config.name,
            Self::BitbucketServer(config) => &config.name,
            Self::Manifest(config) => &config.name,
        }
    }
}

fn resolve_path(base: &Path, path: &Path) -> Result<PathBuf> {
    let expanded = if path == Path::new("~") {
        home_dir().ok_or_else(|| Error::Config("cannot determine home directory".into()))?
    } else if let Ok(rest) = path.strip_prefix("~/") {
        home_dir()
            .ok_or_else(|| Error::Config("cannot determine home directory".into()))?
            .join(rest)
    } else {
        path.to_path_buf()
    };
    Ok(if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    })
}

fn default_config_path() -> Result<PathBuf> {
    let directory = env_path("XDG_CONFIG_HOME")
        .or_else(platform_config_dir)
        .or_else(|| home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| Error::Config("cannot determine configuration directory".into()))?;
    Ok(directory.join("multi-repo/config.toml"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(windows)]
fn platform_config_dir() -> Option<PathBuf> {
    env_path("APPDATA")
}

#[cfg(not(windows))]
const fn platform_config_dir() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn home_dir() -> Option<PathBuf> {
    env_path("USERPROFILE").or_else(|| env_path("HOME"))
}

#[cfg(not(windows))]
fn home_dir() -> Option<PathBuf> {
    env_path("HOME")
}

pub(crate) fn validate_repo_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Config("repository name cannot be empty".into()));
    }
    if name.split('/').any(str::is_empty) || name.contains('\\') {
        return Err(Error::Config(format!("unsafe repository name {name:?}")));
    }
    let path = Path::new(name);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(Error::Config(format!("unsafe repository name {name:?}")));
    }
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(Error::Config(format!("unsafe repository name {name:?}")));
        };
        validate_component(&value.to_string_lossy(), "repository path component")?;
    }
    Ok(())
}

fn validate_component(value: &str, kind: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(Error::Config(format!("invalid {kind} {value:?}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_repo_names() {
        for name in ["", "../escape", "/absolute", "a/../b", "a//b"] {
            assert!(validate_repo_name(name).is_err(), "accepted {name:?}");
        }
        validate_repo_name("org/repo").unwrap();
    }
}
