//! Pure-P2P sync engine powered by Iroh (`iroh`, `iroh-blobs`, `iroh-docs`, `iroh-gossip`).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("P2P sync engine not yet implemented (scheduled for Phase 2)")]
    NotImplemented,
}

/// Placeholder handle for the Iroh P2P node (implemented in Phase 2).
#[derive(Debug, Default)]
pub struct SyncEngine;

impl SyncEngine {
    pub fn new() -> Result<Self, SyncError> {
        Ok(Self)
    }
}
