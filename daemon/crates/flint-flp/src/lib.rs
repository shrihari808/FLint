//! FL Studio `.flp` binary parser and semantic musical diff engine.

use flint_core::{MusicalDiff, ProjectManifest};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FlpError {
    #[error("invalid FLP header magic (expected 'FLhd')")]
    InvalidHeader,
    #[error("unexpected end of FLP data")]
    UnexpectedEof,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parser not yet implemented (scheduled for Phase 1)")]
    NotImplemented,
}

/// Parse an `.flp` binary buffer into a normalized `ProjectManifest`.
pub fn parse_flp_bytes(_bytes: &[u8]) -> Result<ProjectManifest, FlpError> {
    Err(FlpError::NotImplemented)
}

/// Compute a semantic musical diff between two `ProjectManifest` versions.
pub fn diff_manifests(_old: &ProjectManifest, _new: &ProjectManifest) -> MusicalDiff {
    MusicalDiff::default()
}
