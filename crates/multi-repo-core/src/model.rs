use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RepoSpec {
    pub id: String,
    pub canonical_url: String,
    pub clone_url: String,
    pub default_branch: Option<String>,
    pub tags: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RepoRecord {
    pub id: String,
    pub canonical_url: String,
    pub clone_url: String,
    pub local_path: PathBuf,
    pub default_branch: Option<String>,
    pub active: bool,
    pub status: RepoStatus,
    pub sources: BTreeSet<String>,
    pub tags: BTreeSet<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoStatus {
    Pending,
    Ready,
    Error,
}

impl RepoStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Error => "error",
        }
    }

    pub(crate) fn parse(value: &str) -> crate::Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "error" => Ok(Self::Error),
            other => Err(crate::Error::Config(format!(
                "unknown repository status {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CloneProtocol {
    #[default]
    Ssh,
    Https,
}
