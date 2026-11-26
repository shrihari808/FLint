use crate::{SyncError, SyncNode};
use anyhow::Context;
use flint_core::{Comment, CrateItem, PeerEnvironment, ProjectManifest, Snapshot};
use futures_lite::StreamExt;
use iroh::EndpointId;
use iroh_blobs::Hash;
use iroh_docs::{
    api::{
        protocol::{AddrInfoOptions, ShareMode},
        Doc,
    },
    engine::LiveEvent,
    store::{DownloadPolicy, Query},
    Capability, DocTicket, NamespaceId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ulid::Ulid;

const KEY_SESSION_META: &[u8] = b"meta/session";
const KEY_ROTATED_FROM: &[u8] = b"meta/rotated_from";
const PREFIX_PEERS: &[u8] = b"peers/";
const PREFIX_SNAPSHOTS: &[u8] = b"snapshots/";
const PREFIX_COMMENTS: &[u8] = b"comments/";
const PREFIX_CRATE: &[u8] = b"crate/";
/// Sessions created before v0.2.1 registered every file under `blobs/<hash>` in the shared doc.
/// New code no longer writes these (see `ingest_file`), but still reads them for reporting.
const PREFIX_LEGACY_BLOBS: &[u8] = b"blobs/";

/// Name of the hidden folder FLint keeps inside project / pulled folders.
pub const WORKSPACE_DIR: &str = ".flint";

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

/// How an invite ticket is generated.
#[derive(Debug, Clone, Copy, Default)]
pub struct ShareOptions {
    /// Read-only ticket: the holder can sync and download, but never write. Use for seeders.
    pub read_only: bool,
    /// Leave this machine's IP addresses out of the ticket (relay URL + endpoint ID only).
    /// Connections still work, they just start via the relay before hole-punching.
    pub hide_addresses: bool,
}

/// Where a pushed snapshot's parent came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentSource {
    /// `--parent <snapshot_id>` was given.
    Explicit,
    /// Read from `.flint/<session_id>.json` in the project folder (written by pull / last push).
    Workspace,
    /// No workspace record; fell back to the newest snapshot in the session.
    Latest,
    /// First snapshot in the session.
    None,
}

#[derive(Debug, Clone, Default)]
pub struct PushOptions {
    pub note: String,
    pub author_display_name: String,
    /// Explicit parent snapshot ID. Overrides the workspace record.
    pub parent: Option<String>,
    pub preview_audio_path: Option<PathBuf>,
    pub preview_peaks: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResult {
    pub snapshot: Snapshot,
    pub new_blobs: usize,
    pub reused_blobs: usize,
    pub total_bytes_ingested: u64,
    /// Samples referenced by the project that were not found on disk (not uploaded).
    pub missing_samples: Vec<String>,
    pub parent_source: ParentSource,
    /// True when someone else pushed after your parent: this push starts a parallel branch.
    pub diverged_from_latest: bool,
    /// Newest snapshot in the session at push time (before this push).
    pub latest_snapshot_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PullOptions {
    /// Snapshot to pull (`None` = newest).
    pub snapshot_id: Option<String>,
    /// How long to keep trying to download missing files from peers.
    pub wait: Duration,
    /// Rewrite sample paths inside the pulled `.flp` so FL Studio finds the pulled samples.
    pub rewrite_sample_paths: bool,
}

impl Default for PullOptions {
    fn default() -> Self {
        Self {
            snapshot_id: None,
            wait: Duration::from_secs(30),
            rewrite_sample_paths: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResult {
    pub snapshot: Snapshot,
    pub output_dir: PathBuf,
    pub flp_path: PathBuf,
    pub files_written: usize,
    /// Files that had to be downloaded from peers during this pull.
    pub downloaded_blobs: usize,
    /// How many sample paths inside the `.flp` were rewritten to point at the pulled files.
    pub rewritten_sample_paths: usize,
    /// Untouched copy of the `.flp` as pushed (only when paths were rewritten).
    pub original_flp_path: Option<PathBuf>,
}

/// Which snapshots `prefetch` downloads files for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefetchScope {
    Latest,
    All,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrefetchReport {
    pub requested: usize,
    pub already_local: usize,
    pub downloaded: usize,
    /// Hashes no reachable peer could provide before the deadline.
    pub unavailable: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PruneReport {
    pub kept_snapshots: usize,
    pub pruned_snapshots: usize,
    /// FLint tags released; their files are deleted on the next garbage-collection run.
    pub released_blobs: usize,
    /// Files still pinned by legacy `blobs/<hash>` doc entries from sessions created before
    /// v0.2.1. These cannot be freed by prune (use `session rotate` to start a clean session).
    pub legacy_pinned_blobs: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotateResult {
    pub old_session_id: String,
    pub created: SessionCreated,
    pub copied_entries: usize,
}

/// Contents of `.flint/<session_id>.json`: which snapshot a working folder is based on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub session_id: String,
    pub base_snapshot_id: String,
    pub updated_at: u64,
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
            .share(ShareMode::Write, self.default_addr_options(false))
            .await
            .context("failed to generate invite ticket")?;

        Ok(SessionCreated {
            session_id: doc.id().to_string(),
            invite_ticket: ticket.to_string(),
            meta,
        })
    }

    /// Generate a fresh invite ticket for an existing session.
    pub async fn share_session(
        &self,
        session_id: &str,
        options: ShareOptions,
    ) -> Result<String, SyncError> {
        let doc = self.open_session_doc(session_id).await?;
        let mode = if options.read_only {
            ShareMode::Read
        } else {
            ShareMode::Write
        };
        let ticket = doc
            .share(mode, self.default_addr_options(options.hide_addresses))
            .await
            .context("failed to share session")?;
        Ok(ticket.to_string())
    }

    /// Join a session from a collaborator's invite ticket and begin syncing.
    pub async fn join_session(&self, ticket_str: &str) -> Result<String, SyncError> {
        let ticket = parse_ticket(ticket_str)?;
        self.import_ticket(ticket).await
    }

    /// Join a session but only ever with read access, even if given a write ticket.
    /// Used by seeders so a compromised seeder machine cannot rewrite session history.
    pub async fn join_session_read_only(&self, ticket_str: &str) -> Result<(String, String), SyncError> {
        let ticket = parse_ticket(ticket_str)?;
        let read_only = DocTicket {
            capability: Capability::Read(ticket.capability.id()),
            nodes: ticket.nodes,
        };
        let read_only_str = read_only.to_string();
        let sid = self.import_ticket(read_only).await?;
        Ok((sid, read_only_str))
    }

    async fn import_ticket(&self, ticket: DocTicket) -> Result<String, SyncError> {
        // Remember the inviter's addresses so blob downloads can dial them by endpoint ID.
        for addr in &ticket.nodes {
            self.add_peer_addr(addr.clone());
        }
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
        options: PushOptions,
    ) -> Result<PushResult, SyncError> {
        let input_path = flp_or_zip_path.as_ref();
        let doc = self.open_session_doc(session_id).await?;
        let author = self.docs.author_default().await?;
        let project_dir = input_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        // 1. Parse .flp (or zipped loop) into normalized ProjectManifest
        let manifest = flint_flp::parse_flp_path(input_path)?;

        // 2. Decide the parent snapshot. Using "newest in the session" was wrong whenever the
        //    pusher started from an older version: the diff then showed other people's work as
        //    reverted and the branch was hidden. The parent is the version this folder was
        //    pulled from (or last pushed), unless told otherwise.
        let existing_snapshots = self.list_snapshots(session_id).await?;
        let latest_id = existing_snapshots.last().map(|s| s.id.clone());
        let (parent_id, parent_source) = if let Some(explicit) = options.parent.clone() {
            if !existing_snapshots.iter().any(|s| s.id == explicit) {
                return Err(SyncError::SnapshotNotFound(explicit));
            }
            (Some(explicit), ParentSource::Explicit)
        } else if let Some(rec) = read_workspace_record(&project_dir, session_id) {
            (Some(rec.base_snapshot_id), ParentSource::Workspace)
        } else if let Some(latest) = &latest_id {
            (Some(latest.clone()), ParentSource::Latest)
        } else {
            (None, ParentSource::None)
        };

        let parent_snapshot = parent_id
            .as_ref()
            .and_then(|pid| existing_snapshots.iter().find(|s| &s.id == pid));
        let diff_summary = match (&parent_id, parent_snapshot) {
            (None, _) => vec![format!(
                "Initial push ({} channels, {} patterns, {} samples)",
                manifest.channels.len(),
                manifest.patterns.len(),
                manifest.samples.len()
            )],
            (Some(_), Some(parent)) => {
                flint_flp::diff_manifests(&parent.manifest, &manifest).summary_lines
            }
            (Some(pid), None) => vec![format!(
                "Based on snapshot {pid}, which has not synced to this machine yet — diff unavailable"
            )],
        };
        let diverged_from_latest = matches!(
            (&parent_id, &latest_id),
            (Some(p), Some(l)) if p != l
        );

        // 3. Ingest .flp + referenced audio assets into iroh-blobs (deduplicated by BLAKE3)
        let mut stats = IngestStats::default();
        let ingested = self
            .ingest_project_files(session_id, input_path, &manifest, &mut stats)
            .await?;

        // 4. Optional preview audio clip captured by VST3
        let preview_audio_blob = match &options.preview_audio_path {
            Some(preview_path) => {
                let hash = self.ingest_file(session_id, preview_path, &mut stats).await?;
                Some(hash.to_string())
            }
            None => None,
        };

        // 5. Write immutable snapshot record under snapshots/<ulid>_<author_short>
        let snap_id = Ulid::new().to_string();
        let snapshot = Snapshot {
            id: snap_id.clone(),
            parent_ids: parent_id.into_iter().collect(),
            author: options.author_display_name.clone(),
            created_at: unix_now(),
            note: options.note.clone(),
            flp_blob: ingested.flp_hash.to_string(),
            flp_filename: ingested.flp_filename,
            manifest,
            diff_summary,
            assets: ingested.assets,
            preview_audio_blob,
            preview_peaks: options.preview_peaks.clone(),
            author_node_id: self.node_id(),
            missing_samples: ingested.missing_samples.clone(),
            sample_paths: ingested.sample_paths,
        };

        let author_short = &self.node_id()[..8.min(self.node_id().len())];
        let snap_key = format!("snapshots/{snap_id}_{author_short}");
        let snap_bytes = serde_json::to_vec(&snapshot)?;
        doc.set_bytes(author, snap_key.into_bytes(), snap_bytes)
            .await
            .context("failed to write snapshot entry to iroh-docs")?;

        // 6. This folder is now based on the snapshot we just pushed.
        write_workspace_record(&project_dir, session_id, &snapshot.id)?;

        Ok(PushResult {
            snapshot,
            new_blobs: stats.new_blobs,
            reused_blobs: stats.reused_blobs,
            total_bytes_ingested: stats.total_bytes_ingested,
            missing_samples: ingested.missing_samples,
            parent_source,
            diverged_from_latest,
            latest_snapshot_id: latest_id,
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

    /// Materialize a snapshot into a new folder `target_dir/<snapshot_id>/`.
    ///
    /// Missing files are downloaded from collaborators (retrying until `options.wait` runs out),
    /// sample paths inside the `.flp` are rewritten to the pulled copies, and a workspace record
    /// is written so the next push from that folder knows its parent.
    pub async fn pull_snapshot(
        &self,
        session_id: &str,
        target_dir: impl AsRef<Path>,
        options: PullOptions,
    ) -> Result<PullResult, SyncError> {
        let deadline = tokio::time::Instant::now() + options.wait;
        let snapshot = self
            .wait_for_snapshot(session_id, options.snapshot_id.as_deref(), deadline)
            .await?;

        // Make sure every file is on disk before writing anything.
        let downloaded_blobs = self
            .fetch_blobs(
                session_id,
                &snapshot.referenced_blobs(),
                Some(&snapshot.author_node_id),
                deadline,
            )
            .await?;

        // Never write into an existing folder: it may be a previous pull you have been editing.
        let out_dir = unique_dir(&target_dir.as_ref().join(&snapshot.id));
        std::fs::create_dir_all(&out_dir)?;

        let mut files_written = 0usize;
        let mut dest_by_asset_key: BTreeMap<String, PathBuf> = BTreeMap::new();
        for (rel_path, hash_str) in &snapshot.assets {
            let hash = parse_hash(hash_str)?;
            let dest = sanitize_relative_join(&out_dir, rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.export_blob_to_path(hash, &dest).await?;
            dest_by_asset_key.insert(rel_path.clone(), absolute(&dest)?);
            files_written += 1;
        }

        let flp_hash = parse_hash(&snapshot.flp_blob)?;
        let flp_path = out_dir.join(&snapshot.flp_filename);
        self.export_blob_to_path(flp_hash, &flp_path).await?;
        files_written += 1;

        let mut rewritten_sample_paths = 0usize;
        let mut original_flp_path = None;
        if options.rewrite_sample_paths && !snapshot.assets.is_empty() {
            let original = std::fs::read(&flp_path)?;
            let resolver = SamplePathResolver::new(&snapshot, &dest_by_asset_key);
            let (rewritten, count) =
                flint_flp::rewrite_sample_paths(&original, |raw| resolver.resolve(raw))?;
            if count > 0 {
                let backup = out_dir.join(WORKSPACE_DIR).join("original.flp");
                std::fs::create_dir_all(backup.parent().expect("has parent"))?;
                std::fs::write(&backup, &original)?;
                std::fs::write(&flp_path, &rewritten)?;
                original_flp_path = Some(backup);
                rewritten_sample_paths = count;
            }
        }

        write_workspace_record(&out_dir, session_id, &snapshot.id)?;

        Ok(PullResult {
            snapshot,
            output_dir: out_dir,
            flp_path,
            files_written,
            downloaded_blobs,
            rewritten_sample_paths,
            original_flp_path,
        })
    }

    /// Download (and pin) the files of the newest or of every snapshot plus all crate items,
    /// so they are available later even if their author goes offline. Seeders use `All`.
    pub async fn prefetch(
        &self,
        session_id: &str,
        scope: PrefetchScope,
        wait: Duration,
    ) -> Result<PrefetchReport, SyncError> {
        let snapshots = self.list_snapshots(session_id).await?;
        let chosen: Vec<&Snapshot> = match scope {
            PrefetchScope::Latest => snapshots.last().into_iter().collect(),
            PrefetchScope::All => snapshots.iter().collect(),
        };
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        for s in &chosen {
            wanted.extend(s.referenced_blobs());
        }
        for item in self.list_crate_items(session_id).await? {
            wanted.insert(item.blob);
        }

        let mut report = PrefetchReport {
            requested: wanted.len(),
            ..Default::default()
        };
        let mut missing = Vec::new();
        for h in &wanted {
            let hash = parse_hash(h)?;
            self.tag_blob(session_id, hash).await?;
            if self.blobs.blobs().has(hash).await.unwrap_or(false) {
                report.already_local += 1;
            } else {
                missing.push(h.clone());
            }
        }
        if missing.is_empty() {
            return Ok(report);
        }
        let deadline = tokio::time::Instant::now() + wait;
        match self.fetch_blobs(session_id, &missing, None, deadline).await {
            Ok(n) => report.downloaded = n,
            Err(SyncError::BlobsUnavailable(list)) => {
                report.downloaded = missing.len() - list.len();
                report.unavailable = list;
            }
            Err(e) => return Err(e),
        }
        Ok(report)
    }

    /// Release locally stored files that only older snapshots need.
    ///
    /// Keeps the newest `keep_latest` snapshots' files plus every crate item. Snapshot records
    /// (history, notes, diffs) are never deleted and other collaborators are not affected: this
    /// only frees space on this machine. Pulling a pruned snapshot later re-downloads its files
    /// from any peer that still has them. Files are deleted on the next garbage-collection run.
    pub async fn prune(
        &self,
        session_id: &str,
        keep_latest: usize,
        dry_run: bool,
    ) -> Result<PruneReport, SyncError> {
        let keep_latest = keep_latest.max(1);
        let doc = self.open_session_doc(session_id).await?;
        let snapshots = self.list_snapshots(session_id).await?;
        let split = snapshots.len().saturating_sub(keep_latest);
        let (old, kept) = snapshots.split_at(split);

        let mut keep: HashSet<String> = HashSet::new();
        for s in kept {
            keep.extend(s.referenced_blobs());
        }
        for item in self.list_crate_items(session_id).await? {
            keep.insert(item.blob);
        }

        let prefix = session_tag_prefix(session_id);
        let tags = self
            .blobs
            .tags()
            .list_prefix(prefix.as_bytes())
            .await
            .context("failed to list blob tags")?;
        tokio::pin!(tags);
        let mut to_release = Vec::new();
        while let Some(item) = tags.next().await {
            let info = item.context("failed to read blob tag")?;
            if !keep.contains(&info.hash.to_string()) {
                to_release.push(info.name);
            }
        }

        if !dry_run {
            for name in &to_release {
                self.blobs
                    .tags()
                    .delete(name)
                    .await
                    .context("failed to delete blob tag")?;
            }
        }

        let legacy = doc.get_many(Query::key_prefix(PREFIX_LEGACY_BLOBS)).await?;
        tokio::pin!(legacy);
        let mut legacy_pinned = HashSet::new();
        while let Some(Ok(entry)) = legacy.next().await {
            let h = entry.content_hash().to_string();
            if !keep.contains(&h) {
                legacy_pinned.insert(h);
            }
        }

        Ok(PruneReport {
            kept_snapshots: kept.len(),
            pruned_snapshots: old.len(),
            released_blobs: to_release.len(),
            legacy_pinned_blobs: legacy_pinned.len(),
            dry_run,
        })
    }

    /// Start a fresh session containing this session's history, and stop syncing the old one.
    ///
    /// iroh-docs has no way to revoke a ticket, so this is how you remove a collaborator:
    /// rotate, then send the new ticket only to the people who should keep access. Everyone
    /// left in the old session can still read what was there before the rotation.
    pub async fn rotate_session(
        &self,
        old_session_id: &str,
        author_display_name: &str,
    ) -> Result<RotateResult, SyncError> {
        let old_doc = self.open_session_doc(old_session_id).await?;
        let old_meta = self.get_session_meta(old_session_id).await?;
        let name = old_meta
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "FLint session".to_string());

        let created = self.create_session(&name, author_display_name).await?;
        let new_doc = self.open_session_doc(&created.session_id).await?;
        let author = self.docs.author_default().await?;

        let entries = old_doc.get_many(Query::all()).await?;
        tokio::pin!(entries);
        let mut latest_by_key: BTreeMap<Vec<u8>, (u64, Hash)> = BTreeMap::new();
        while let Some(Ok(entry)) = entries.next().await {
            let key = entry.key().to_vec();
            if key.starts_with(PREFIX_LEGACY_BLOBS)
                || key.starts_with(PREFIX_PEERS)
                || key.as_slice() == KEY_SESSION_META
            {
                continue;
            }
            let ts = entry.timestamp();
            let hash = entry.content_hash();
            match latest_by_key.get(&key) {
                Some((existing_ts, _)) if *existing_ts >= ts => {}
                _ => {
                    latest_by_key.insert(key, (ts, hash));
                }
            }
        }

        let mut copied = 0usize;
        for (key, (_, hash)) in latest_by_key {
            let Ok(bytes) = self.blobs.blobs().get_bytes(hash).await else {
                continue;
            };
            new_doc
                .set_bytes(author, key, bytes)
                .await
                .context("failed to copy entry into rotated session")?;
            copied += 1;
        }
        new_doc
            .set_bytes(
                author,
                KEY_ROTATED_FROM.to_vec(),
                old_session_id.as_bytes().to_vec(),
            )
            .await
            .context("failed to record rotation")?;

        // Keep every file we hold pinned under the new session too.
        for snap in self.list_snapshots(&created.session_id).await? {
            for h in snap.referenced_blobs() {
                if let Ok(hash) = parse_hash(&h) {
                    if self.blobs.blobs().has(hash).await.unwrap_or(false) {
                        self.tag_blob(&created.session_id, hash).await?;
                    }
                }
            }
        }

        old_doc.leave().await.context("failed to leave old session")?;

        Ok(RotateResult {
            old_session_id: old_session_id.to_string(),
            created,
            copied_entries: copied,
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
        let mut stats = IngestStats::default();
        let hash = self
            .ingest_file(session_id, file_path.as_ref(), &mut stats)
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

    /// Materialize a single blob into `<cache_dir>/<hash_prefix>_<filename>` for VST3 playback or
    /// drag-out, downloading it from session peers first if needed.
    pub async fn materialize_blob(
        &self,
        session_id: &str,
        hash_str: &str,
        cache_dir: impl AsRef<Path>,
        filename: &str,
        wait: Duration,
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
            let deadline = tokio::time::Instant::now() + wait;
            self.fetch_blobs(session_id, &[hash_str.to_string()], None, deadline)
                .await?;
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

    /// Wait until the session's metadata has synced from a peer (or `timeout` elapses).
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

    /// Pin a session for `--seeder` mode and join it immediately.
    ///
    /// Unless `allow_write` is set the ticket is downgraded to read-only before it is used or
    /// stored, so the seeder can never write to (or be used to tamper with) the session.
    pub async fn add_seeder_ticket(
        &self,
        ticket_str: &str,
        allow_write: bool,
    ) -> Result<String, SyncError> {
        let (session_id, stored_ticket) = if allow_write {
            (self.join_session(ticket_str).await?, ticket_str.trim().to_string())
        } else {
            self.join_session_read_only(ticket_str).await?
        };
        let mut tickets = self.load_seeder_tickets()?;
        if !tickets.iter().any(|t| *t == stored_ticket) {
            tickets.push(stored_ticket);
            let path = self.data_dir.join("seeder_tickets.json");
            std::fs::write(path, serde_json::to_string_pretty(&tickets)?)?;
        }
        Ok(session_id)
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    fn default_addr_options(&self, hide_addresses: bool) -> AddrInfoOptions {
        // With relays disabled (local-only mode) direct addresses are the only way in.
        if hide_addresses && self.network == crate::NetworkMode::Internet {
            AddrInfoOptions::Relay
        } else {
            AddrInfoOptions::RelayAndAddresses
        }
    }

    async fn open_session_doc(&self, session_id: &str) -> Result<Doc, SyncError> {
        let ns = NamespaceId::from_str(session_id.trim())
            .map_err(|e| SyncError::InvalidSessionId(session_id.to_string(), e.to_string()))?;
        self.docs
            .open(ns)
            .await
            .context("failed to open session doc")?
            .ok_or_else(|| SyncError::SessionNotFound(session_id.to_string()))
    }

    /// Wait until the requested snapshot (or any snapshot) has synced, up to `deadline`.
    async fn wait_for_snapshot(
        &self,
        session_id: &str,
        snapshot_id: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<Snapshot, SyncError> {
        loop {
            let snapshots = self.list_snapshots(session_id).await?;
            let found = match snapshot_id {
                Some(id) => snapshots.into_iter().find(|s| s.id == id),
                None => snapshots.into_iter().last(),
            };
            if let Some(s) = found {
                return Ok(s);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(SyncError::SnapshotNotFound(
                    snapshot_id
                        .map(str::to_string)
                        .unwrap_or_else(|| "no snapshots in session".into()),
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Endpoint IDs that may hold this session's files: the preferred node first (usually the
    /// snapshot's author), then every collaborator that published an environment record, then
    /// every peer we have synced the doc with (this includes read-only seeders).
    async fn providers_for(&self, session_id: &str, preferred: Option<&str>) -> Vec<EndpointId> {
        let me = self.endpoint_id();
        let mut out: Vec<EndpointId> = Vec::new();
        let push = |id: EndpointId, out: &mut Vec<EndpointId>| {
            if id != me && !out.contains(&id) {
                out.push(id);
            }
        };
        if let Some(id) = preferred.and_then(|s| EndpointId::from_str(s).ok()) {
            push(id, &mut out);
        }
        if let Ok(envs) = self.list_peer_envs(session_id).await {
            for env in envs {
                if let Ok(id) = EndpointId::from_str(&env.node_id) {
                    push(id, &mut out);
                }
            }
        }
        if let Ok(doc) = self.open_session_doc(session_id).await {
            if let Ok(Some(peers)) = doc.get_sync_peers().await {
                for bytes in peers {
                    if let Ok(id) = EndpointId::from_bytes(&bytes) {
                        push(id, &mut out);
                    }
                }
            }
        }
        out
    }

    /// Make sure every hash in `hashes` is stored locally and pinned for this session,
    /// downloading missing ones from session peers and retrying until `deadline`.
    /// Returns how many were downloaded.
    async fn fetch_blobs(
        &self,
        session_id: &str,
        hashes: &[String],
        preferred_provider: Option<&str>,
        deadline: tokio::time::Instant,
    ) -> Result<usize, SyncError> {
        let mut missing: Vec<Hash> = Vec::new();
        for h in hashes {
            let hash = parse_hash(h)?;
            // Pin first so GC can never delete it between download and use.
            self.tag_blob(session_id, hash).await?;
            if !self.blobs.blobs().has(hash).await.unwrap_or(false) {
                missing.push(hash);
            }
        }
        let total_missing = missing.len();

        while !missing.is_empty() {
            let providers = self
                .providers_for(session_id, preferred_provider.filter(|s| !s.is_empty()))
                .await;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if !providers.is_empty() && !remaining.is_zero() {
                let request = self.downloader.download(missing.clone(), providers);
                // The download itself can fail or time out; we just re-check what arrived.
                let _ = tokio::time::timeout(remaining, request).await;
            }
            let mut still_missing = Vec::new();
            for hash in missing {
                if !self.blobs.blobs().has(hash).await.unwrap_or(false) {
                    still_missing.push(hash);
                }
            }
            missing = still_missing;
            if missing.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(SyncError::BlobsUnavailable(
                    missing.iter().map(|h| h.to_string()).collect(),
                ));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(total_missing)
    }

    async fn tag_blob(&self, session_id: &str, hash: Hash) -> Result<(), SyncError> {
        self.blobs
            .tags()
            .set(session_tag(session_id, &hash), hash)
            .await
            .context("failed to tag blob")?;
        Ok(())
    }

    async fn ingest_project_files(
        &self,
        session_id: &str,
        input_path: &Path,
        manifest: &ProjectManifest,
        stats: &mut IngestStats,
    ) -> Result<IngestedProject, SyncError> {
        let is_zip = input_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
        if is_zip {
            self.ingest_zip(session_id, input_path, manifest, stats).await
        } else {
            self.ingest_flp_folder(session_id, input_path, manifest, stats)
                .await
        }
    }

    /// Zipped loop package: every file in the zip is ingested at its own relative path, so two
    /// files with the same name in different folders no longer overwrite each other.
    async fn ingest_zip(
        &self,
        session_id: &str,
        zip_path: &Path,
        manifest: &ProjectManifest,
        stats: &mut IngestStats,
    ) -> Result<IngestedProject, SyncError> {
        let file = File::open(zip_path)?;
        let mut archive =
            zip::ZipArchive::new(BufReader::new(file)).map_err(flint_flp::FlpError::Zip)?;

        // The project file is the shallowest .flp in the archive.
        let mut flp_entry: Option<(usize, String)> = None;
        for i in 0..archive.len() {
            let entry = archive.by_index(i).map_err(flint_flp::FlpError::Zip)?;
            if entry.is_dir() {
                continue;
            }
            let Some(key) = sanitize_rel_key(entry.name()) else {
                continue;
            };
            if key.to_ascii_lowercase().ends_with(".flp") {
                let depth = key.matches('/').count();
                let better = match &flp_entry {
                    None => true,
                    Some((_, existing)) => depth < existing.matches('/').count(),
                };
                if better {
                    flp_entry = Some((i, key));
                }
            }
        }
        let (flp_index, flp_key) = flp_entry.ok_or(flint_flp::FlpError::NoFlpInZip)?;
        // Paths are stored relative to the folder containing the .flp.
        let flp_dir_prefix = match flp_key.rfind('/') {
            Some(i) => format!("{}/", &flp_key[..i]),
            None => String::new(),
        };
        let flp_filename = flp_key.rsplit('/').next().unwrap_or(&flp_key).to_string();

        let mut flp_hash = None;
        let mut assets = BTreeMap::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(flint_flp::FlpError::Zip)?;
            if entry.is_dir() {
                continue;
            }
            let Some(key) = sanitize_rel_key(entry.name()) else {
                continue;
            };
            let tmp = self.data_dir.join("tmp").join(Ulid::new().to_string());
            {
                let mut out = File::create(&tmp)?;
                std::io::copy(&mut entry, &mut out)?;
            }
            let result = self.ingest_file(session_id, &tmp, stats).await;
            let _ = std::fs::remove_file(&tmp);
            let hash = result?;

            if i == flp_index {
                flp_hash = Some(hash);
            } else {
                let rel = key
                    .strip_prefix(&flp_dir_prefix)
                    .map(str::to_string)
                    .unwrap_or(key);
                assets.insert(rel, hash.to_string());
            }
        }

        // Map each sample path written in the .flp to the zip file with the same name.
        let mut by_basename: BTreeMap<String, String> = BTreeMap::new();
        for key in assets.keys() {
            by_basename
                .entry(basename(key).to_ascii_lowercase())
                .or_insert_with(|| key.clone());
        }
        let mut sample_paths = BTreeMap::new();
        let mut missing_samples = Vec::new();
        for raw in &manifest.samples {
            match by_basename.get(&basename(raw).to_ascii_lowercase()) {
                Some(key) => {
                    sample_paths.insert(raw.clone(), key.clone());
                }
                None => missing_samples.push(raw.clone()),
            }
        }

        Ok(IngestedProject {
            flp_filename,
            flp_hash: flp_hash.ok_or(flint_flp::FlpError::NoFlpInZip)?,
            assets,
            sample_paths,
            missing_samples,
        })
    }

    /// Unpacked `.flp`: ingest the project file plus every referenced sample we can find.
    async fn ingest_flp_folder(
        &self,
        session_id: &str,
        flp_path: &Path,
        manifest: &ProjectManifest,
        stats: &mut IngestStats,
    ) -> Result<IngestedProject, SyncError> {
        let flp_filename = flp_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("project.flp")
            .to_string();
        let flp_hash = self.ingest_file(session_id, flp_path, stats).await?;

        let project_dir = flp_path.parent().unwrap_or_else(|| Path::new("."));
        let project_dir_abs = absolute(project_dir)?;
        let mut assets: BTreeMap<String, String> = BTreeMap::new();
        let mut sample_paths = BTreeMap::new();
        let mut missing_samples = Vec::new();

        for raw_sample in &manifest.samples {
            let Some(resolved) =
                resolve_sample_path(raw_sample, project_dir, manifest.data_path.as_deref())
            else {
                missing_samples.push(raw_sample.clone());
                continue;
            };
            let hash = self.ingest_file(session_id, &resolved, stats).await?;
            let hash_str = hash.to_string();

            // Keep the folder layout for samples inside the project folder; put outside samples
            // under Samples/. Never let two different files share a key.
            let mut key = absolute(&resolved)?
                .strip_prefix(&project_dir_abs)
                .ok()
                .and_then(|rel| sanitize_rel_key(&rel.to_string_lossy()))
                .unwrap_or_else(|| format!("Samples/{}", basename(&resolved.to_string_lossy())));
            if assets.get(&key).is_some_and(|existing| *existing != hash_str) {
                key = format!(
                    "Samples/{}/{}",
                    &hash_str[..8],
                    basename(&resolved.to_string_lossy())
                );
            }
            assets.insert(key.clone(), hash_str);
            sample_paths.insert(raw_sample.clone(), key);
        }

        Ok(IngestedProject {
            flp_filename,
            flp_hash,
            assets,
            sample_paths,
            missing_samples,
        })
    }

    /// Add one file to the blob store by streaming it from disk (never loading it whole into
    /// memory), and pin it for this session. Already-stored files are only hashed, not copied.
    async fn ingest_file(
        &self,
        session_id: &str,
        path: &Path,
        stats: &mut IngestStats,
    ) -> Result<Hash, SyncError> {
        let size = std::fs::metadata(path)?.len();
        let mut hasher = blake3::Hasher::new();
        {
            let mut reader = BufReader::new(File::open(path)?);
            hasher.update_reader(&mut reader)?;
        }
        let hash = Hash::from(hasher.finalize());

        if self.blobs.blobs().has(hash).await.unwrap_or(false) {
            self.tag_blob(session_id, hash).await?;
            stats.reused_blobs += 1;
            return Ok(hash);
        }

        let stored = self
            .blobs
            .blobs()
            .add_path(absolute(path)?)
            .with_named_tag(session_tag(session_id, &hash))
            .await
            .context("failed to add file to iroh-blobs store")?;
        debug_assert_eq!(stored.hash, hash);
        stats.new_blobs += 1;
        stats.total_bytes_ingested += size;
        Ok(stored.hash)
    }

    async fn export_blob_to_path(&self, hash: Hash, dest: &Path) -> Result<(), SyncError> {
        if !self.blobs.blobs().has(hash).await.unwrap_or(false) {
            return Err(SyncError::BlobUnavailable(hash.to_string()));
        }
        let abs_dest = absolute(dest)?;
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

#[derive(Debug, Default)]
struct IngestStats {
    new_blobs: usize,
    reused_blobs: usize,
    total_bytes_ingested: u64,
}

struct IngestedProject {
    flp_filename: String,
    flp_hash: Hash,
    assets: BTreeMap<String, String>,
    sample_paths: BTreeMap<String, String>,
    missing_samples: Vec<String>,
}

/// Maps a sample path stored in the `.flp` to the absolute path of the pulled copy.
struct SamplePathResolver {
    exact: BTreeMap<String, String>,
    by_basename: BTreeMap<String, String>,
}

impl SamplePathResolver {
    fn new(snapshot: &Snapshot, dest_by_asset_key: &BTreeMap<String, PathBuf>) -> Self {
        let to_str = |p: &PathBuf| p.to_string_lossy().to_string();
        let mut exact = BTreeMap::new();
        for (raw, key) in &snapshot.sample_paths {
            if let Some(dest) = dest_by_asset_key.get(key) {
                exact.insert(raw.clone(), to_str(dest));
            }
        }
        // Snapshots pushed before `sample_paths` existed: fall back to matching file names.
        let mut by_basename = BTreeMap::new();
        for (key, dest) in dest_by_asset_key {
            by_basename
                .entry(basename(key).to_ascii_lowercase())
                .or_insert_with(|| to_str(dest));
        }
        Self { exact, by_basename }
    }

    fn resolve(&self, raw: &str) -> Option<String> {
        self.exact
            .get(raw)
            .or_else(|| self.by_basename.get(&basename(raw).to_ascii_lowercase()))
            .cloned()
    }
}

fn parse_ticket(ticket_str: &str) -> Result<DocTicket, SyncError> {
    DocTicket::from_str(ticket_str.trim()).map_err(|e| SyncError::InvalidTicket(e.to_string()))
}

fn session_tag_prefix(session_id: &str) -> String {
    format!("flint/s/{}/", session_id.trim())
}

fn session_tag(session_id: &str, hash: &Hash) -> String {
    format!("{}{hash}", session_tag_prefix(session_id))
}

fn workspace_record_path(dir: &Path, session_id: &str) -> PathBuf {
    dir.join(WORKSPACE_DIR)
        .join(format!("{}.json", session_id.trim()))
}

/// Read which snapshot the folder `dir` is based on, for this session.
pub fn read_workspace_record(dir: &Path, session_id: &str) -> Option<WorkspaceRecord> {
    let raw = std::fs::read(workspace_record_path(dir, session_id)).ok()?;
    let rec: WorkspaceRecord = serde_json::from_slice(&raw).ok()?;
    (rec.session_id == session_id.trim()).then_some(rec)
}

fn write_workspace_record(dir: &Path, session_id: &str, snapshot_id: &str) -> Result<(), SyncError> {
    let path = workspace_record_path(dir, session_id);
    std::fs::create_dir_all(path.parent().expect("has parent"))?;
    let rec = WorkspaceRecord {
        session_id: session_id.trim().to_string(),
        base_snapshot_id: snapshot_id.to_string(),
        updated_at: unix_now(),
    };
    std::fs::write(path, serde_json::to_vec_pretty(&rec)?)?;
    Ok(())
}

/// `dir`, or `dir-2`, `dir-3`, ... — the first one that does not exist yet.
fn unique_dir(dir: &Path) -> PathBuf {
    if !dir.exists() {
        return dir.to_path_buf();
    }
    let name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    (2..)
        .map(|n| dir.with_file_name(format!("{name}-{n}")))
        .find(|candidate| !candidate.exists())
        .expect("unbounded search")
}

fn absolute(path: &Path) -> Result<PathBuf, SyncError> {
    Ok(std::path::absolute(path)?)
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
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

/// Normalize a relative path from a zip or folder into a safe `a/b/c` key
/// (no empty, `.`, `..`, or drive-letter segments). `None` if nothing is left.
fn sanitize_rel_key(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw
        .split(['/', '\\'])
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != ".." && !seg.contains(':'))
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn sanitize_relative_join(root: &Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for seg in rel.split(['/', '\\']) {
        if seg.is_empty() || seg == "." || seg == ".." || seg.contains(':') {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_zip_paths() {
        assert_eq!(sanitize_rel_key("Samples/Kick.wav").as_deref(), Some("Samples/Kick.wav"));
        assert_eq!(sanitize_rel_key("..\\..\\evil.wav").as_deref(), Some("evil.wav"));
        assert_eq!(sanitize_rel_key("C:\\x\\y.wav").as_deref(), Some("x/y.wav"));
        assert_eq!(sanitize_rel_key("./"), None);
    }

    #[test]
    fn unique_dir_never_reuses_an_existing_folder() {
        let base = std::env::temp_dir().join(format!("flint_unique_{}", Ulid::new()));
        std::fs::create_dir_all(&base).unwrap();
        let next = unique_dir(&base);
        assert_ne!(next, base);
        assert!(next.to_string_lossy().ends_with("-2"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn workspace_record_round_trips() {
        let dir = std::env::temp_dir().join(format!("flint_ws_{}", Ulid::new()));
        std::fs::create_dir_all(&dir).unwrap();
        write_workspace_record(&dir, "sess", "01SNAP").unwrap();
        let rec = read_workspace_record(&dir, "sess").unwrap();
        assert_eq!(rec.base_snapshot_id, "01SNAP");
        assert!(read_workspace_record(&dir, "other").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
