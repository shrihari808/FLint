//! Shared domain types for FLint (manifests, musical diffs, snapshots, comments, and crate items).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Normalized summary extracted from an FL Studio `.flp` project file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub fl_version: String,
    pub title: String,
    pub data_path: Option<String>,
    pub bpm: f64,
    pub time_sig_num: u8,
    pub time_sig_den: u8,
    pub ppq: u16,
    pub channels: Vec<ChannelInfo>,
    pub mixer_inserts: Vec<MixerInsertInfo>,
    pub patterns: Vec<PatternInfo>,
    pub samples: Vec<String>,
}

impl Default for ProjectManifest {
    fn default() -> Self {
        Self {
            fl_version: String::new(),
            title: String::new(),
            data_path: None,
            bpm: 130.0,
            time_sig_num: 4,
            time_sig_den: 4,
            ppq: 96,
            channels: Vec::new(),
            mixer_inserts: Vec::new(),
            patterns: Vec::new(),
            samples: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub index: u16,
    pub name: String,
    /// Native plugin or "Fruity Wrapper" (if hosting a third-party VST).
    pub plugin_name: Option<String>,
    /// Resolved third-party VST2/VST3 name when hosted inside Fruity Wrapper.
    pub wrapped_plugin_name: Option<String>,
    pub sample_path: Option<String>,
}

impl ChannelInfo {
    /// Returns the effective plugin name (preferring the unwrapped VST name when present).
    pub fn effective_plugin_name(&self) -> Option<&str> {
        self.wrapped_plugin_name
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.plugin_name.as_deref().filter(|s| !s.is_empty()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MixerInsertInfo {
    pub index: u16,
    pub name: String,
    pub fx_plugins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternInfo {
    pub index: u16,
    pub name: String,
    pub note_count: usize,
    pub notes_hash: String,
}

/// Semantic musical diff between two `ProjectManifest` versions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MusicalDiff {
    pub summary_lines: Vec<String>,
    pub bpm_changed: Option<(f64, f64)>,
    pub time_sig_changed: Option<((u8, u8), (u8, u8))>,
    pub fl_version_changed: Option<(String, String)>,
    pub added_channels: Vec<String>,
    pub removed_channels: Vec<String>,
    pub added_plugins: Vec<String>,
    pub removed_plugins: Vec<String>,
    pub added_patterns: Vec<String>,
    pub removed_patterns: Vec<String>,
    pub modified_patterns: Vec<String>,
    pub added_samples: Vec<String>,
    pub removed_samples: Vec<String>,
}

impl MusicalDiff {
    pub fn is_empty(&self) -> bool {
        self.summary_lines.is_empty()
    }
}

/// Environment advertised by each collaborator in the P2P session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerEnvironment {
    pub node_id: String,
    pub display_name: String,
    pub fl_version: Option<String>,
    pub installed_plugins: Vec<String>,
    pub updated_at: u64,
}

/// Result of checking a project's plugins against a collaborator's environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub peer_display_name: String,
    pub missing_plugins: Vec<MissingPluginWarning>,
    pub fl_version_mismatch: Option<String>,
}

impl CompatibilityReport {
    pub fn is_compatible(&self) -> bool {
        self.missing_plugins.is_empty() && self.fl_version_mismatch.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingPluginWarning {
    pub plugin_name: String,
    pub location: String,
}

/// A versioned project snapshot stored in `iroh-docs` referencing BLAKE3 blobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub parent_ids: Vec<String>,
    pub author: String,
    pub created_at: u64,
    pub note: String,
    pub flp_blob: String,
    pub flp_filename: String,
    pub manifest: ProjectManifest,
    pub diff_summary: Vec<String>,
    /// Map of project-relative sample path -> BLAKE3 hash in `iroh-blobs`.
    pub assets: BTreeMap<String, String>,
    pub preview_audio_blob: Option<String>,
    pub preview_peaks: Vec<f32>,
}

/// A bar/beat-locked mix comment pinned to a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub snapshot_id: String,
    pub author: String,
    pub bar: u32,
    pub beat: f32,
    pub time_sec: f64,
    pub text: String,
    pub resolved: bool,
    pub created_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CrateItemKind {
    Stem,
    Midi,
    Preset,
}

/// A stem, MIDI clip, or preset shared in the live drag-and-drop crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrateItem {
    pub id: String,
    pub author: String,
    pub kind: CrateItemKind,
    pub name: String,
    pub bpm: Option<f64>,
    pub start_bar: Option<u32>,
    pub length_bars: Option<u32>,
    pub source_track: Option<String>,
    pub blob: String,
    pub peaks: Vec<f32>,
    pub created_at: u64,
}

/// Real-time events pushed from `flint-daemon` to `FLint.vst3` over WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonEvent {
    PeerPresence {
        session_id: String,
        node_id: String,
        display_name: String,
        online: bool,
    },
    SnapshotReceived {
        session_id: String,
        snapshot_id: String,
    },
    SyncProgress {
        session_id: String,
        blob_hash: String,
        bytes_done: u64,
        bytes_total: u64,
    },
    CommentAdded {
        session_id: String,
        comment_id: String,
    },
    CrateItemAdded {
        session_id: String,
        item_id: String,
    },
    FlpModifiedOnDisk {
        session_id: String,
        flp_path: String,
        diff_summary: Vec<String>,
    },
}
