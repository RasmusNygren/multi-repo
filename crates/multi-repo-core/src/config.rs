use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::CloneProtocol;

pub const CONFIG_VERSION: u32 = 1;
pub const CONFIG_FILE_NAME: &str = ".multi-repo.toml";
pub(crate) const WORKSPACE_SOURCE_NAME: &str = "workspace";

#[derive(Clone, Debug)]
pub struct Config {
    pub version: u32,
    pub root: PathBuf,
    pub sources: Vec<SourceConfig>,
    pub repositories: Vec<RepositoryConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u32,
    root: Option<PathBuf>,
    #[serde(default, rename = "source")]
    sources: Vec<SourceConfig>,
    #[serde(default, rename = "repo")]
    repositories: Vec<RepositoryConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SourceConfig {
    #[serde(rename = "github")]
    GitHub(GitHubConfig),
    BitbucketServer(BitbucketServerConfig),
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

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    pub id: String,
    pub url: String,
    pub default_branch: Option<String>,
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
    /// Loads and validates configuration from `path` or the nearest workspace.
    ///
    /// Without an explicit path, the current directory and its ancestors are
    /// searched for [`.multi-repo.toml`](CONFIG_FILE_NAME). Relative paths in
    /// the configuration are resolved relative to that file. If `root` is
    /// omitted, the directory containing the configuration is the workspace
    /// root.
    ///
    /// # Errors
    ///
    /// Returns an error if the current directory cannot be determined, no
    /// workspace configuration is found, the file cannot be read or parsed,
    /// or the configuration is invalid.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => resolve_path(&current_dir()?, path)?,
            None => find_config(&current_dir()?)?,
        };
        let contents = std::fs::read_to_string(&path).map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        let file: ConfigFile = toml::from_str(&contents)?;
        if file.version != CONFIG_VERSION {
            return Err(Error::Config(format!(
                "unsupported config version {}; expected {CONFIG_VERSION}",
                file.version
            )));
        }

        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let mut config = Self {
            version: file.version,
            root: match file.root {
                Some(root) => resolve_path(base, &root)?,
                None => base.to_path_buf(),
            },
            sources: file.sources,
            repositories: file.repositories,
        };
        let mut names = HashSet::new();
        for source in &mut config.sources {
            let name = source.name();
            validate_component(name, "source name")?;
            if name == WORKSPACE_SOURCE_NAME {
                return Err(Error::Config(format!(
                    "source name {WORKSPACE_SOURCE_NAME:?} is reserved for repositories declared directly in {CONFIG_FILE_NAME}"
                )));
            }
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
        let mut repository_ids = HashSet::new();
        for repository in &mut config.repositories {
            validate_repo_name(&repository.id)?;
            if !repository_ids.insert(&repository.id) {
                return Err(Error::Config(format!(
                    "duplicate repository id {:?} in {CONFIG_FILE_NAME}",
                    repository.id
                )));
            }
            repository.url = resolve_repository_url(base, &repository.url)?;
        }
        Ok(config)
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

pub(crate) fn resolve_repository_url(base: &Path, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(Error::Config("repository URL cannot be empty".into()));
    }
    let path = Path::new(value);
    let is_scp_remote = value
        .split_once(':')
        .is_some_and(|(host, remote_path)| !host.contains(['/', '\\']) && !remote_path.is_empty());
    if value.contains("://") || (!path.is_absolute() && is_scp_remote) {
        return Ok(value.to_owned());
    }
    resolve_path(base, path)?
        .into_os_string()
        .into_string()
        .map_err(|path| {
            Error::Config(format!(
                "repository path is not valid UTF-8: {}",
                PathBuf::from(path).display()
            ))
        })
}

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().map_err(Error::CurrentDir)
}

fn find_config(start: &Path) -> Result<PathBuf> {
    for directory in start.ancestors() {
        let candidate = directory.join(CONFIG_FILE_NAME);
        match std::fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => return Ok(candidate),
            Ok(_) => {
                return Err(Error::Config(format!(
                    "workspace configuration {} is not a regular file",
                    candidate.display()
                )));
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Read {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Err(Error::Config(format!(
        "no {CONFIG_FILE_NAME} found in {} or its parents; use --config PATH to select one explicitly",
        start.display()
    )))
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
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
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn discovers_nearest_workspace() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let nested = workspace.join("repos/source/org/repo");
        std::fs::create_dir_all(&nested).unwrap();
        let path = workspace.join(CONFIG_FILE_NAME);
        std::fs::write(&path, "version = 1\n").unwrap();

        assert_eq!(find_config(&nested).unwrap(), path);
        assert_eq!(Config::load(Some(&path)).unwrap().root, workspace);

        std::fs::write(&path, "version = 1\nroot = \"managed\"\n").unwrap();
        assert_eq!(
            Config::load(Some(&path)).unwrap().root,
            workspace.join("managed")
        );

        let child_path = workspace.join("repos/source").join(CONFIG_FILE_NAME);
        std::fs::write(&child_path, "version = 1\n").unwrap();
        assert_eq!(find_config(&nested).unwrap(), child_path);
    }

    #[test]
    fn reports_when_no_workspace_exists() {
        let temp = TempDir::new().unwrap();
        let error = find_config(temp.path()).unwrap_err().to_string();
        assert!(error.contains(CONFIG_FILE_NAME));
        assert!(error.contains("--config"));
    }

    #[test]
    fn resolves_inline_repository_paths_from_the_workspace_config() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let path = workspace.join(CONFIG_FILE_NAME);
        std::fs::write(
            &path,
            "version = 1\n\n[[repo]]\nid = \"widget\"\nurl = \"local/widget.git\"\n",
        )
        .unwrap();

        let config = Config::load(Some(&path)).unwrap();

        assert_eq!(
            config.repositories[0].url,
            workspace.join("local/widget.git").to_string_lossy()
        );
    }

    #[test]
    fn parses_minimal_github_source() {
        let file: ConfigFile = toml::from_str(
            "version = 1\n\n[[source]]\nname = \"github\"\nkind = \"github\"\ntoken_env = \"GITHUB_TOKEN\"\n",
        )
        .unwrap();

        assert!(matches!(file.sources.as_slice(), [SourceConfig::GitHub(_)]));
    }

    #[test]
    fn rejects_unsafe_repo_names() {
        for name in ["", "../escape", "/absolute", "a/../b", "a//b"] {
            assert!(validate_repo_name(name).is_err(), "accepted {name:?}");
        }
        validate_repo_name("org/repo").unwrap();
    }
}
