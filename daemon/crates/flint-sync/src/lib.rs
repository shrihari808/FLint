//! Pure-P2P multi-writer sync engine powered by Iroh (`iroh`, `iroh-blobs`, `iroh-docs`, `iroh-gossip`).

mod node;
mod session;

pub use node::{NetworkMode, NodeOptions, SyncNode};
pub use session::{
    read_workspace_record, ParentSource, PrefetchReport, PrefetchScope, PruneReport,
    PullOptions, PullResult, PushOptions, PushResult, RotateResult, SessionCreated, SessionMeta,
    ShareOptions, WorkspaceRecord, WORKSPACE_DIR,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),
    #[error("invalid session ID '{0}': {1}")]
    InvalidSessionId(String, String),
    #[error("invalid invite ticket: {0}")]
    InvalidTicket(String),
    #[error("invalid blob hash '{0}': {1}")]
    InvalidHash(String, String),
    #[error("blob {0} is not yet available locally or from connected peers")]
    BlobUnavailable(String),
    #[error(
        "{} file(s) could not be downloaded: no online collaborator or seeder has them. \
         Ask the author to open FLint (or run a seeder), then try again. Missing: {}",
        .0.len(),
        .0.join(", ")
    )]
    BlobsUnavailable(Vec<String>),
    #[error("FLP parse error: {0}")]
    Flp(#[from] flint_flp::FlpError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("sync engine error: {0}")]
    Other(#[from] anyhow::Error),
}
