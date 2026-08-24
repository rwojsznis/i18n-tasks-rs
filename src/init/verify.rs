//! Reads the generated config back.
//!
//! The command offers a config it has already parsed and loaded, so the header
//! can report the keys the settings actually read instead of promising them.

use crate::config::Config;
use crate::data::load::Store;
use crate::stats::forest_stats;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What the generated config read back.
#[derive(Debug, Clone, PartialEq)]
pub struct Verification {
    pub locales: Vec<String>,
    pub key_count: usize,
    pub files_read: usize,
    /// Set when the generated config did not load. The file is still produced:
    /// a config that needs one edit beats no config at all.
    pub error: Option<String>,
}

/// Reads the generated config back and loads the data with it.
pub(super) fn verify(output: &str, root: &Path, to: &Path) -> Verification {
    let empty = Verification {
        locales: Vec::new(),
        key_count: 0,
        files_read: 0,
        error: None,
    };
    let cfg = match Config::parse(output, to, root.to_path_buf()) {
        Ok(cfg) => cfg,
        Err(e) => {
            return Verification {
                error: Some(e),
                ..empty
            };
        }
    };
    let store = match Store::load(&cfg) {
        Ok(store) => store,
        Err(e) => {
            return Verification {
                error: Some(e),
                ..empty
            };
        }
    };
    let files: BTreeSet<&Path> = store
        .trees
        .values()
        .flat_map(|t| t.file_locales.keys().map(PathBuf::as_path))
        .collect();
    Verification {
        locales: store.locales.clone(),
        key_count: forest_stats(&store, &store.locales).key_count,
        files_read: files.len(),
        error: None,
    }
}
