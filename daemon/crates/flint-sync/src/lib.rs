//! Pure-P2P multi-writer sync engine powered by Iroh (`iroh`, `iroh-blobs`, `iroh-docs`, `iroh-gossip`).

mod node;
mod session;

pub use node::SyncNode;
pub use session::{PullResult, PushResult, SessionCreated, SessionMeta};

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
    #[error("FLP parse error: {0}")]
    Flp(#[from] flint_flp::FlpError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("sync engine error: {0}")]
    Other(#[from] anyhow::Error),
}
