use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SetManifest {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,

    #[serde(default)]
    pub created_at: u64,

    #[serde(default)]
    pub scopes: Vec<String>,

    #[serde(default)]
    pub exclusions: Vec<String>,

    #[serde(default)]
    pub deleted_paths: Vec<String>, 
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub hash: String,
    pub size: u64,
    pub modified: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DiffSummary {
    pub new_files: usize,
    pub modified_files: usize,
    pub deleted_files: usize,
    pub is_dirty: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BlkConfig {
    pub path_map: HashMap<String, PathBuf>,
}