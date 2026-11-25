use crate::SyncError;
use anyhow::Context;
use iroh::{
    address_lookup::memory::MemoryLookup, endpoint::presets, protocol::Router, Endpoint,
    EndpointAddr, EndpointId, RelayMode, SecretKey,
};
use iroh_blobs::{
    api::downloader::Downloader,
    store::{
        fs::{options::Options as FsOptions, FsStore},
        GcConfig,
    },
    BlobsProtocol, ALPN as BLOBS_ALPN,
};
use iroh_docs::{engine::ProtectCallbackHandler, protocol::Docs, ALPN as DOCS_ALPN};
use iroh_gossip::{net::Gossip, ALPN as GOSSIP_ALPN};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How the node reaches other machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkMode {
    /// Normal operation: n0 public relays + DNS/pkarr address lookup, so peers on different
    /// networks can find each other by endpoint ID.
    #[default]
    Internet,
    /// Loopback only (127.0.0.1), no relays, no address lookup services. Peers must be told
    /// each other's addresses (invite tickets carry them, or call [`SyncNode::add_peer_addr`]).
    /// Used by tests so they pass without internet access.
    LocalOnly,
}

/// Options for [`SyncNode::start_with_options`].
#[derive(Debug, Clone)]
pub struct NodeOptions {
    pub network: NetworkMode,
    /// How often blob garbage collection runs. Only blobs that are neither tagged by FLint nor
    /// referenced by any `iroh-docs` entry are ever deleted.
    pub gc_interval: Duration,
}

impl Default for NodeOptions {
    fn default() -> Self {
        Self {
            network: NetworkMode::Internet,
            gc_interval: Duration::from_secs(10 * 60),
        }
    }
}

/// Persistent local Iroh P2P node hosting `iroh-blobs`, `iroh-gossip`, and `iroh-docs`.
pub struct SyncNode {
    pub(crate) data_dir: PathBuf,
    pub(crate) endpoint: Endpoint,
    pub(crate) blobs: FsStore,
    pub(crate) docs: Docs,
    pub(crate) downloader: Downloader,
    pub(crate) network: NetworkMode,
    lookup: MemoryLookup,
    router: Router,
}

impl SyncNode {
    /// Start a persistent P2P sync node backed by `<data_dir>/{secret.key, blobs/, docs/}`.
    pub async fn start(data_dir: impl AsRef<Path>) -> Result<Self, SyncError> {
        Self::start_with_options(data_dir, NodeOptions::default()).await
    }

    /// Start a node with explicit network / garbage-collection options.
    pub async fn start_with_options(
        data_dir: impl AsRef<Path>,
        options: NodeOptions,
    ) -> Result<Self, SyncError> {
        let data_dir = data_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(data_dir.join("tmp"))?;

        let secret_key = load_or_create_secret_key(&data_dir.join("secret.key"))?;

        // In-memory address book. Addresses from invite tickets are added here so that blob
        // downloads can dial a collaborator by endpoint ID even without DNS/pkarr lookup.
        let lookup = MemoryLookup::new();

        let builder = match options.network {
            NetworkMode::Internet => Endpoint::builder(presets::N0),
            NetworkMode::LocalOnly => Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .bind_addr("127.0.0.1:0")
                .context("invalid loopback bind address")?,
        };
        let endpoint = builder
            .secret_key(secret_key)
            .address_lookup(lookup.clone())
            .bind()
            .await
            .context("failed to bind Iroh QUIC endpoint")?;

        // Garbage collection: a blob is kept if FLint holds a tag for it OR any iroh-docs entry
        // references it (the docs engine reports those through the protect callback). Everything
        // else is deleted on the next GC run. This is what makes `session prune` free disk space.
        let (protect_handler, protect_cb) = ProtectCallbackHandler::new();
        let blobs_dir = data_dir.join("blobs");
        std::fs::create_dir_all(&blobs_dir)?;
        let mut fs_options = FsOptions::new(&blobs_dir);
        fs_options.gc = Some(GcConfig {
            interval: options.gc_interval,
            add_protected: Some(protect_cb),
        });
        let blobs = FsStore::load_with_opts(blobs_dir.join("blobs.db"), fs_options)
            .await
            .context("failed to open persistent iroh-blobs FsStore")?;

        let gossip = Gossip::builder().spawn(endpoint.clone());

        let docs_dir = data_dir.join("docs");
        std::fs::create_dir_all(&docs_dir)?;
        let docs = Docs::persistent(docs_dir)
            .protect_handler(protect_handler)
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await
            .context("failed to spawn persistent iroh-docs engine")?;

        let downloader = blobs.downloader(&endpoint);

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
            downloader,
            network: options.network,
            lookup,
            router,
        })
    }

    /// Wait up to `timeout` for the node's relay/public address to come online.
    /// Returns immediately in [`NetworkMode::LocalOnly`] (there is no relay to wait for).
    pub async fn wait_online(&self, timeout: Duration) {
        if self.network == NetworkMode::LocalOnly {
            return;
        }
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

    /// This node's currently known addresses (for handing to another node in tests).
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// Remember how to reach another node (used for tickets and in local-only tests).
    pub fn add_peer_addr(&self, addr: EndpointAddr) {
        self.lookup.add_endpoint_info(addr);
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
