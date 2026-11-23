use crate::{SyncError, SyncNode};
use anyhow::Context;
use flint_core::{Comment, CrateItem, PeerEnvironment, ProjectManifest, Snapshot};
use futures_lite::StreamExt;
use iroh_blobs::Hash;
use iroh_docs::{
    api::{
        protocol::{AddrInfoOptions, ShareMode},
        Doc,
    },
    engine::LiveEvent,
    store::{DownloadPolicy, Query},
    DocTicket, NamespaceId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ulid::Ulid;

const KEY_SESSION_META: &[u8] = b"meta/session";
const PREFIX_PEERS: &[u8] = b"peers/";
const PREFIX_SNAPSHOTS: &[u8] = b"snapshots/";
const PREFIX_COMMENTS: &[u8] = b"comments/";
const PREFIX_CRATE: &[u8] = b"crate/";
const PREFIX_BLOBS: &[u8] = b"blobs/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub name: String,
    pub created_by: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreated {
    pub session_id: String,
    pub invite_ticket: String,
    pub meta: SessionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResult {
    pub snapshot: Snapshot,
    pub new_blobs: usize,
    pub reused_blobs: usize,
    pub total_bytes_ingested: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResult {
    pub snapshot: Snapshot,
    pub output_dir: PathBuf,
    pub flp_path: PathBuf,
    pub files_written: usize,
}

impl SyncNode {
    /// Create a new multi-writer FLint collaboration session and return its shareable invite ticket.
    pub async fn create_session(
        &self,
        name: &str,
        author_display_name: &str,
    ) -> Result<SessionCreated, SyncError> {
        let doc = self
            .docs
            .create()
            .await
            .context("failed to create iroh-docs namespace")?;
        doc.set_download_policy(DownloadPolicy::default())
            .await
            .context("failed to set download policy")?;

        let author = self
            .docs
            .author_default()
            .await
            .context("failed to get default doc author")?;

        let meta = SessionMeta {
            name: name.to_string(),
            created_by: author_display_name.to_string(),
            created_at: unix_now(),
        };
        let meta_bytes = serde_json::to_vec(&meta)?;
        doc.set_bytes(author, KEY_SESSION_META.to_vec(), meta_bytes)
            .await
            .context("failed to write session metadata")?;

        let ticket = doc
            .share(ShareMode::Write, AddrInfoOptions::RelayAndAddresses)
            .await
            .context("failed to generate invite ticket")?;

        Ok(SessionCreated {
            session_id: doc.id().to_string(),
            invite_ticket: ticket.to_string(),
            meta,
        })
    }

    /// Generate a fresh write-enabled invite ticket for an existing session.
    pub async fn share_session(&self, session_id: &str) -> Result<String, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let ticket = doc
            .share(ShareMode::Write, AddrInfoOptions::RelayAndAddresses)
            .await
            .context("failed to share session")?;
        Ok(ticket.to_string())
    }

    /// Join a session from a collaborator's invite ticket and begin syncing.
    pub async fn join_session(&self, ticket_str: &str) -> Result<String, SyncError> {
        let ticket = DocTicket::from_str(ticket_str.trim())
            .map_err(|e| SyncError::InvalidTicket(e.to_string()))?;
        let doc = self
            .docs
            .import(ticket)
            .await
            .context("failed to import session ticket")?;
        doc.set_download_policy(DownloadPolicy::default())
            .await
            .context("failed to set download policy")?;
        Ok(doc.id().to_string())
    }

    /// Read the session metadata (`meta/session`) if synced.
    pub async fn get_session_meta(&self, session_id: &str) -> Result<Option<SessionMeta>, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let Some(entry) = doc
            .get_one(Query::key_exact(KEY_SESSION_META))
            .await
            .context("failed to query session metadata")?
        else {
            return Ok(None);
        };
        let bytes = self
            .blobs
            .blobs()
            .get_bytes(entry.content_hash())
            .await
            .context("failed to read session metadata blob")?;
        let meta: SessionMeta = serde_json::from_slice(&bytes)?;
        Ok(Some(meta))
    }

    /// Publish this machine's FL Studio version & installed VST list into the session.
    pub async fn publish_peer_env(
        &self,
        session_id: &str,
        env: &PeerEnvironment,
    ) -> Result<(), SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let author = self.docs.author_default().await?;
        let key = format!("peers/{}/env", self.node_id());
        let bytes = serde_json::to_vec(env)?;
        doc.set_bytes(author, key.into_bytes(), bytes)
            .await
            .context("failed to publish peer environment")?;
        Ok(())
    }

    /// List advertised environments of all collaborators in a session.
    pub async fn list_peer_envs(&self, session_id: &str) -> Result<Vec<PeerEnvironment>, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let entries = doc.get_many(Query::key_prefix(PREFIX_PEERS)).await?;
        tokio::pin!(entries);
        let mut by_key: BTreeMap<Vec<u8>, PeerEnvironment> = BTreeMap::new();
        while let Some(Ok(entry)) = entries.next().await {
            if let Ok(bytes) = self.blobs.blobs().get_bytes(entry.content_hash()).await {
                if let Ok(env) = serde_json::from_slice::<PeerEnvironment>(&bytes) {
                    by_key.insert(entry.key().to_vec(), env);
                }
            }
        }
        Ok(by_key.into_values().collect())
    }

    /// Push a new deduplicated project snapshot (`.flp` folder or `.zip` loop package).
    pub async fn push_project(
        &self,
        session_id: &str,
        flp_or_zip_path: impl AsRef<Path>,
        note: &str,
        author_display_name: &str,
        preview_audio_path: Option<&Path>,
        preview_peaks: Vec<f32>,
    ) -> Result<PushResult, SyncError> {
        let input_path = flp_or_zip_path.as_ref();
        let doc = self.open_session_doc(session_id).await?;
        let author = self.docs.author_default().await?;

        // 1. Parse .flp (or zipped loop) into normalized ProjectManifest
        let manifest = flint_flp::parse_flp_path(input_path)?;

        // 2. Compute parent_ids & automatic semantic musical diff against latest snapshot
        let existing_snapshots = self.list_snapshots(session_id).await?;
        let (parent_ids, diff_summary) = if let Some(latest) = existing_snapshots.last() {
            let diff = flint_flp::diff_manifests(&latest.manifest, &manifest);
            (vec![latest.id.clone()], diff.summary_lines)
        } else {
            (
                Vec::new(),
                vec![format!(
                    "Initial push ({} channels, {} patterns, {} samples)",
                    manifest.channels.len(),
                    manifest.patterns.len(),
                    manifest.samples.len()
                )],
            )
        };

        // 3. Collect existing blob keys in this doc to accurately report dedup reuse
        let mut registered_blobs = self.list_registered_blob_hashes(&doc).await?;
        let mut new_blobs = 0usize;
        let mut reused_blobs = 0usize;
        let mut total_bytes_ingested = 0u64;

        // 4. Ingest .flp + referenced audio assets into iroh-blobs (deduplicated by BLAKE3)
        let (flp_filename, flp_hash, assets) = self
            .ingest_project_files(
                &doc,
                author,
                input_path,
                &manifest,
                &mut registered_blobs,
                &mut new_blobs,
                &mut reused_blobs,
                &mut total_bytes_ingested,
            )
            .await?;

        // 5. Optional preview audio clip captured by VST3
        let preview_audio_blob = if let Some(preview_path) = preview_audio_path {
            let bytes = std::fs::read(preview_path)?;
            let hash = self
                .ingest_blob_bytes(
                    &doc,
                    author,
                    bytes,
                    &mut registered_blobs,
                    &mut new_blobs,
                    &mut reused_blobs,
                    &mut total_bytes_ingested,
                )
                .await?;
            Some(hash.to_string())
        } else {
            None
        };

        // 6. Write immutable snapshot record under snapshots/<ulid>_<author_short>
        let snap_id = Ulid::new().to_string();
        let snapshot = Snapshot {
            id: snap_id.clone(),
            parent_ids,
            author: author_display_name.to_string(),
            created_at: unix_now(),
            note: note.to_string(),
            flp_blob: flp_hash.to_string(),
            flp_filename,
            manifest,
            diff_summary,
            assets,
            preview_audio_blob,
            preview_peaks,
        };

        let author_short = &self.node_id()[..8.min(self.node_id().len())];
        let snap_key = format!("snapshots/{snap_id}_{author_short}");
        let snap_bytes = serde_json::to_vec(&snapshot)?;
        doc.set_bytes(author, snap_key.into_bytes(), snap_bytes)
            .await
            .context("failed to write snapshot entry to iroh-docs")?;

        Ok(PushResult {
            snapshot,
            new_blobs,
            reused_blobs,
            total_bytes_ingested,
        })
    }

    /// List all snapshots in chronological order (including concurrent multi-writer branches).
    pub async fn list_snapshots(&self, session_id: &str) -> Result<Vec<Snapshot>, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let entries = doc.get_many(Query::key_prefix(PREFIX_SNAPSHOTS)).await?;
        tokio::pin!(entries);
        let mut by_key: BTreeMap<String, Snapshot> = BTreeMap::new();
        while let Some(Ok(entry)) = entries.next().await {
            if let Ok(bytes) = self.blobs.blobs().get_bytes(entry.content_hash()).await {
                if let Ok(snap) = serde_json::from_slice::<Snapshot>(&bytes) {
                    let key_str = String::from_utf8_lossy(entry.key()).to_string();
                    by_key.insert(key_str, snap);
                }
            }
        }
        Ok(by_key.into_values().collect())
    }

    /// Materialize a snapshot (latest if `snapshot_id` is `None`) cleanly into `target_dir/<snapshot_id>/`.
    pub async fn pull_snapshot(
        &self,
        session_id: &str,
        snapshot_id: Option<&str>,
        target_dir: impl AsRef<Path>,
    ) -> Result<PullResult, SyncError> {
        let snapshots = self.list_snapshots(session_id).await?;
        let snapshot = match snapshot_id {
            Some(id) => snapshots
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| SyncError::SnapshotNotFound(id.to_string()))?,
            None => snapshots
                .into_iter()
                .last()
                .ok_or_else(|| SyncError::SnapshotNotFound("no snapshots in session".into()))?,
        };

        let out_dir = target_dir.as_ref().join(&snapshot.id);
        std::fs::create_dir_all(&out_dir)?;

        let flp_hash = parse_hash(&snapshot.flp_blob)?;
        let flp_path = out_dir.join(&snapshot.flp_filename);
        self.export_blob_to_path(flp_hash, &flp_path).await?;
        let mut files_written = 1usize;

        for (rel_path, hash_str) in &snapshot.assets {
            let hash = parse_hash(hash_str)?;
            let dest = sanitize_relative_join(&out_dir, rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.export_blob_to_path(hash, &dest).await?;
            files_written += 1;
        }

        Ok(PullResult {
            snapshot,
            output_dir: out_dir,
            flp_path,
            files_written,
        })
    }

    /// Add or update a bar/beat-locked comment on a snapshot.
    pub async fn upsert_comment(&self, session_id: &str, comment: &Comment) -> Result<(), SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let author = self.docs.author_default().await?;
        let key = format!("comments/{}/{}", comment.snapshot_id, comment.id);
        let bytes = serde_json::to_vec(comment)?;
        doc.set_bytes(author, key.into_bytes(), bytes)
            .await
            .context("failed to write comment")?;
        Ok(())
    }

    /// List comments for a session (optionally filtered to one snapshot).
    pub async fn list_comments(
        &self,
        session_id: &str,
        snapshot_id: Option<&str>,
    ) -> Result<Vec<Comment>, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let prefix = match snapshot_id {
            Some(sid) => format!("comments/{sid}/").into_bytes(),
            None => PREFIX_COMMENTS.to_vec(),
        };
        let entries = doc.get_many(Query::key_prefix(prefix)).await?;
        tokio::pin!(entries);
        let mut by_key: BTreeMap<String, Comment> = BTreeMap::new();
        while let Some(Ok(entry)) = entries.next().await {
            if let Ok(bytes) = self.blobs.blobs().get_bytes(entry.content_hash()).await {
                if let Ok(comment) = serde_json::from_slice::<Comment>(&bytes) {
                    by_key.insert(String::from_utf8_lossy(entry.key()).to_string(), comment);
                }
            }
        }
        Ok(by_key.into_values().collect())
    }

    /// Share a stem, MIDI clip, or preset into the session's live crate.
    pub async fn add_crate_item(
        &self,
        session_id: &str,
        file_path: impl AsRef<Path>,
        mut item: CrateItem,
    ) -> Result<CrateItem, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let author = self.docs.author_default().await?;
        let bytes = std::fs::read(file_path.as_ref())?;

        let mut registered = self.list_registered_blob_hashes(&doc).await?;
        let (mut nb, mut rb, mut total) = (0, 0, 0);
        let hash = self
            .ingest_blob_bytes(&doc, author, bytes, &mut registered, &mut nb, &mut rb, &mut total)
            .await?;

        if item.id.is_empty() {
            item.id = Ulid::new().to_string();
        }
        if item.created_at == 0 {
            item.created_at = unix_now();
        }
        item.blob = hash.to_string();

        let key = format!("crate/{}", item.id);
        let item_bytes = serde_json::to_vec(&item)?;
        doc.set_bytes(author, key.into_bytes(), item_bytes)
            .await
            .context("failed to write crate item")?;
        Ok(item)
    }

    /// List all items in the session's shared stem/MIDI/preset crate.
    pub async fn list_crate_items(&self, session_id: &str) -> Result<Vec<CrateItem>, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let entries = doc.get_many(Query::key_prefix(PREFIX_CRATE)).await?;
        tokio::pin!(entries);
        let mut by_key: BTreeMap<String, CrateItem> = BTreeMap::new();
        while let Some(Ok(entry)) = entries.next().await {
            if let Ok(bytes) = self.blobs.blobs().get_bytes(entry.content_hash()).await {
                if let Ok(item) = serde_json::from_slice::<CrateItem>(&bytes) {
                    by_key.insert(String::from_utf8_lossy(entry.key()).to_string(), item);
                }
            }
        }
        Ok(by_key.into_values().collect())
    }

    /// Materialize a single blob into `<cache_dir>/<hash_prefix>_<filename>` for VST3 playback or drag-out.
    pub async fn materialize_blob(
        &self,
        hash_str: &str,
        cache_dir: impl AsRef<Path>,
        filename: &str,
    ) -> Result<PathBuf, SyncError> {
        let hash = parse_hash(hash_str)?;
        std::fs::create_dir_all(cache_dir.as_ref())?;
        let safe_name = Path::new(filename)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("blob.bin");
        let dest = cache_dir
            .as_ref()
            .join(format!("{}_{}", &hash_str[..8.min(hash_str.len())], safe_name));
        if !dest.exists() {
            self.export_blob_to_path(hash, &dest).await?;
        }
        Ok(dest)
    }

    /// Subscribe to live P2P events (neighbor presence, remote inserts, content ready) for a session.
    pub async fn subscribe_session(
        &self,
        session_id: &str,
    ) -> Result<impl futures_lite::Stream<Item = anyhow::Result<LiveEvent>>, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let stream = doc
            .subscribe()
            .await
            .context("failed to subscribe to session events")?;
        Ok(stream)
    }

    /// Wait until at least one snapshot (and its blobs) syncs or `timeout` elapses.
    pub async fn wait_for_sync_ready(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<(), SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let events = doc
            .subscribe()
            .await
            .context("failed to subscribe while waiting for sync")?;
        tokio::pin!(events);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, events.next()).await {
                Ok(Some(Ok(LiveEvent::PendingContentReady))) => break,
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        Ok(())
    }

    /// Load pinned session tickets for `--seeder` mode from `<data_dir>/seeder_tickets.json`.
    pub fn load_seeder_tickets(&self) -> Result<Vec<String>, SyncError> {
        let path = self.data_dir.join("seeder_tickets.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Persist a new session ticket for `--seeder` mode and join it immediately.
    pub async fn add_seeder_ticket(&self, ticket_str: &str) -> Result<String, SyncError> {
        let session_id = self.join_session(ticket_str).await?;
        let mut tickets = self.load_seeder_tickets()?;
        if !tickets.iter().any(|t| t == ticket_str.trim()) {
            tickets.push(ticket_str.trim().to_string());
            let path = self.data_dir.join("seeder_tickets.json");
            std::fs::write(path, serde_json::to_string_pretty(&tickets)?)?;
        }
        Ok(session_id)
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    async fn open_session_doc(&self, session_id: &str) -> Result<Doc, SyncError> {
        let ns = NamespaceId::from_str(session_id.trim())
            .map_err(|e| SyncError::InvalidSessionId(session_id.to_string(), e.to_string()))?;
        self.docs
            .open(ns)
            .await
            .context("failed to open session doc")?
            .ok_or_else(|| SyncError::SessionNotFound(session_id.to_string()))
    }

    async fn list_registered_blob_hashes(&self, doc: &Doc) -> Result<HashSet<String>, SyncError> {
        let entries = doc.get_many(Query::key_prefix(PREFIX_BLOBS)).await?;
        tokio::pin!(entries);
        let mut hashes = HashSet::new();
        while let Some(Ok(entry)) = entries.next().await {
            hashes.insert(entry.content_hash().to_string());
        }
        Ok(hashes)
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_project_files(
        &self,
        doc: &Doc,
        author: iroh_docs::AuthorId,
        input_path: &Path,
        manifest: &ProjectManifest,
        registered_blobs: &mut HashSet<String>,
        new_blobs: &mut usize,
        reused_blobs: &mut usize,
        total_bytes_ingested: &mut u64,
    ) -> Result<(String, Hash, BTreeMap<String, String>), SyncError> {
        let is_zip = input_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"));

        let mut assets = BTreeMap::new();

        if is_zip {
            let file = File::open(input_path)?;
            let mut archive = zip::ZipArchive::new(BufReader::new(file))
                .map_err(flint_flp::FlpError::Zip)?;
            let mut flp_info: Option<(String, Hash)> = None;

            for i in 0..archive.len() {
                let mut entry = archive.by_index(i).map_err(flint_flp::FlpError::Zip)?;
                if entry.is_dir() {
                    continue;
                }
                let entry_name = entry.name().to_string();
                let basename = entry_name
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&entry_name)
                    .to_string();
                let mut bytes = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut bytes)?;

                let hash = self
                    .ingest_blob_bytes(
                        doc,
                        author,
                        bytes,
                        registered_blobs,
                        new_blobs,
                        reused_blobs,
                        total_bytes_ingested,
                    )
                    .await?;

                if basename.to_ascii_lowercase().ends_with(".flp") && flp_info.is_none() {
                    flp_info = Some((basename, hash));
                } else if !basename.is_empty() {
                    assets.insert(format!("Samples/{basename}"), hash.to_string());
                }
            }

            let (flp_filename, flp_hash) =
                flp_info.ok_or(flint_flp::FlpError::NoFlpInZip)?;
            return Ok((flp_filename, flp_hash, assets));
        }

        // Unpacked .flp project folder: ingest .flp + resolve each referenced sample individually
        let flp_filename = input_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project.flp")
            .to_string();
        let flp_bytes = std::fs::read(input_path)?;
        let flp_hash = self
            .ingest_blob_bytes(
                doc,
                author,
                flp_bytes,
                registered_blobs,
                new_blobs,
                reused_blobs,
                total_bytes_ingested,
            )
            .await?;

        let project_dir = input_path.parent().unwrap_or_else(|| Path::new("."));
        for raw_sample in &manifest.samples {
            if let Some(resolved) = resolve_sample_path(raw_sample, project_dir, manifest.data_path.as_deref()) {
                let basename = resolved
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("sample.wav")
                    .to_string();
                let bytes = std::fs::read(&resolved)?;
                let hash = self
                    .ingest_blob_bytes(
                        doc,
                        author,
                        bytes,
                        registered_blobs,
                        new_blobs,
                        reused_blobs,
                        total_bytes_ingested,
                    )
                    .await?;
                assets.insert(format!("Samples/{basename}"), hash.to_string());
            }
        }

        Ok((flp_filename, flp_hash, assets))
    }

    #[allow(clippy::too_many_arguments)]
    async fn ingest_blob_bytes(
        &self,
        doc: &Doc,
        author: iroh_docs::AuthorId,
        bytes: Vec<u8>,
        registered_blobs: &mut HashSet<String>,
        new_blobs: &mut usize,
        reused_blobs: &mut usize,
        total_bytes_ingested: &mut u64,
    ) -> Result<Hash, SyncError> {
        let size = bytes.len() as u64;
        let expected_hash = Hash::from(blake3::hash(&bytes));
        let expected_str = expected_hash.to_string();

        if registered_blobs.contains(&expected_str)
            && self.blobs.blobs().has(expected_hash).await.unwrap_or(false)
        {
            *reused_blobs += 1;
            return Ok(expected_hash);
        }

        let tag = self
            .blobs
            .blobs()
            .add_bytes(bytes)
            .with_tag()
            .await
            .context("failed to add blob to iroh-blobs store")?;
        let hash = tag.hash;
        let hash_str = hash.to_string();

        if registered_blobs.insert(hash_str.clone()) {
            let blob_key = format!("blobs/{hash_str}");
            doc.set_hash(author, blob_key.into_bytes(), hash, size)
                .await
                .context("failed to register blob hash in iroh-docs")?;
            *new_blobs += 1;
            *total_bytes_ingested += size;
        } else {
            *reused_blobs += 1;
        }

        Ok(hash)
    }

    async fn export_blob_to_path(&self, hash: Hash, dest: &Path) -> Result<(), SyncError> {
        if !self.blobs.blobs().has(hash).await.unwrap_or(false) {
            return Err(SyncError::BlobUnavailable(hash.to_string()));
        }
        let abs_dest = if dest.is_absolute() {
            dest.to_path_buf()
        } else {
            std::env::current_dir()?.join(dest)
        };
        if abs_dest.exists() {
            std::fs::remove_file(&abs_dest)?;
        }
        self.blobs
            .blobs()
            .export(hash, abs_dest)
            .await
            .context("failed to export blob to disk")?;
        Ok(())
    }
}

fn resolve_sample_path(
    raw_sample: &str,
    project_dir: &Path,
    data_path: Option<&str>,
) -> Option<PathBuf> {
    let direct = PathBuf::from(raw_sample);
    if direct.is_file() {
        return Some(direct);
    }
    let basename = raw_sample.rsplit(['\\', '/']).next()?;
    let candidates = [
        project_dir.join("Samples").join(basename),
        project_dir.join(basename),
    ];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    if let Some(dp) = data_path {
        let dp_path = Path::new(dp);
        for c in [dp_path.join("Samples").join(basename), dp_path.join(basename)] {
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

fn sanitize_relative_join(root: &Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for seg in rel.split(['/', '\\']) {
        if seg.is_empty() || seg == "." || seg == ".." {
            continue;
        }
        out.push(seg);
    }
    out
}

fn parse_hash(hex: &str) -> Result<Hash, SyncError> {
    Hash::from_str(hex.trim())
        .map_err(|e| SyncError::InvalidHash(hex.to_string(), e.to_string()))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
