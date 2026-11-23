use crate::SyncError;
use anyhow::Context;
use iroh::{endpoint::presets, protocol::Router, Endpoint, EndpointId, SecretKey};
use iroh_blobs::{store::fs::FsStore, BlobsProtocol, ALPN as BLOBS_ALPN};
use iroh_docs::{protocol::Docs, ALPN as DOCS_ALPN};
use iroh_gossip::{net::Gossip, ALPN as GOSSIP_ALPN};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Persistent local Iroh P2P node hosting `iroh-blobs`, `iroh-gossip`, and `iroh-docs`.
pub struct SyncNode {
    pub(crate) data_dir: PathBuf,
    pub(crate) endpoint: Endpoint,
    pub(crate) blobs: FsStore,
    pub(crate) docs: Docs,
    router: Router,
}

impl SyncNode {
    /// Start a persistent P2P sync node backed by `<data_dir>/{secret.key, blobs/, docs/}`.
    pub async fn start(data_dir: impl AsRef<Path>) -> Result<Self, SyncError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;

        let secret_key = load_or_create_secret_key(&data_dir.join("secret.key"))?;

        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .bind()
            .await
            .context("failed to bind Iroh QUIC endpoint")?;

        let blobs_dir = data_dir.join("blobs");
        std::fs::create_dir_all(&blobs_dir)?;
        let blobs = FsStore::load(&blobs_dir)
            .await
            .context("failed to open persistent iroh-blobs FsStore")?;

        let gossip = Gossip::builder().spawn(endpoint.clone());

        let docs_dir = data_dir.join("docs");
        std::fs::create_dir_all(&docs_dir)?;
        let docs = Docs::persistent(docs_dir)
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await
            .context("failed to spawn persistent iroh-docs engine")?;

        let router = Router::builder(endpoint.clone())
            .accept(BLOBS_ALPN, BlobsProtocol::new(&blobs, None))
            .accept(GOSSIP_ALPN, gossip)
            .accept(DOCS_ALPN, docs.clone())
            .spawn();

        Ok(Self {
            data_dir,
            endpoint,
            blobs,
            docs,
            router,
        })
    }

    /// Wait up to `timeout` for the node's relay/public address to come online.
    /// Safe to call even in offline/LAN environments (simply returns after `timeout`).
    pub async fn wait_online(&self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.endpoint.online()).await;
    }

    /// This node's persistent Ed25519 `EndpointId`.
    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// String representation of this node's persistent `EndpointId`.
    pub fn node_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Root data directory for this node.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Access the underlying `iroh-docs` protocol handle.
    pub fn docs(&self) -> &Docs {
        &self.docs
    }

    /// Access the underlying `iroh-blobs` persistent store.
    pub fn blobs(&self) -> &FsStore {
        &self.blobs
    }

    /// Gracefully shut down the router (which also tears down docs, gossip, and blobs handlers).
    pub async fn shutdown(self) -> Result<(), SyncError> {
        self.router
            .shutdown()
            .await
            .context("failed to shut down Iroh router")?;
        // BlobsProtocol shutdown already closes the underlying FsStore; ignore if already closed.
        let _ = self.blobs.shutdown().await;
        Ok(())
    }
}

fn load_or_create_secret_key(path: &Path) -> Result<SecretKey, SyncError> {
    if path.exists() {
        let raw = std::fs::read(path)?;
        if raw.len() == 32 {
            let bytes: [u8; 32] = raw.try_into().expect("checked length 32");
            return Ok(SecretKey::from_bytes(&bytes));
        }
    }
    let key = SecretKey::generate();
    std::fs::write(path, key.to_bytes())?;
    Ok(key)
}
