#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleManifest {
    pub key: String,
    pub name: String,
    pub version: String,
    pub permissions: Vec<String>,
    pub capabilities: Vec<String>,
}

pub trait PlatformModule: Send + Sync {
    fn manifest(&self) -> ModuleManifest;
}
