//! Persistent local daemon + CLI + optional `--seeder` entrypoint for FLint.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use flint_core::PeerEnvironment;
use flint_sync::{
    NodeOptions, ParentSource, PrefetchScope, PullOptions, PushOptions, ShareOptions, SyncNode,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(
    name = "flint-daemon",
    version,
    about = "FLint — DAW-aware P2P collaboration daemon & FL Studio project analyzer"
)]
struct Cli {
    /// Override FLint local storage directory (defaults to OS app data directory)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Parse and inspect an FL Studio .flp file (or zipped loop .zip)
    Inspect {
        /// Path to a .flp or .zip file
        path: PathBuf,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Compute a semantic musical diff between two .flp (or .zip) project versions
    Diff {
        /// Older project version (.flp or .zip)
        old: PathBuf,
        /// Newer project version (.flp or .zip)
        new: PathBuf,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Scan installed VST3/VST2/CLAP plugins on this machine
    ScanPlugins {
        /// Additional plugin directories to scan
        #[arg(long = "extra-dir")]
        extra_dirs: Vec<PathBuf>,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Check a project's third-party plugin & FL version compatibility against a peer (or this machine)
    Check {
        /// Path to a .flp or .zip file
        path: PathBuf,
        /// Collaborator display name
        #[arg(long, default_value = "Local Machine")]
        peer_name: String,
        /// Comma-separated list of peer's installed plugins (defaults to scanning this machine)
        #[arg(long)]
        peer_plugins: Option<String>,
        /// Optional peer FL Studio version (e.g. "24.1.0")
        #[arg(long)]
        peer_fl_version: Option<String>,
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Show or set your display name (saved once, used for every push / comment)
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Manage pure-P2P multi-writer collaboration sessions
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Run a headless always-on P2P seeder node for 24/7 offline async replication
    Seeder {
        /// Pin and join one or more session invite tickets
        #[arg(long = "add")]
        add_tickets: Vec<String>,
        /// Keep write access if a write ticket is given (default: downgrade to read-only)
        #[arg(long)]
        allow_write: bool,
        /// Seconds between full prefetch passes
        #[arg(long, default_value_t = 60)]
        prefetch_interval_secs: u64,
    },
}

#[derive(Subcommand, Debug)]
enum ProfileAction {
    /// Print the saved profile
    Show,
    /// Save your display name
    Set {
        /// Name collaborators see on your snapshots and comments
        #[arg(long)]
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum SessionAction {
    /// Create a new P2P session and print its shareable invite ticket
    Create {
        /// Human-friendly session / track name
        name: String,
        /// Display name (defaults to your saved profile)
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print a fresh invite ticket for an existing session
    Share {
        session_id: String,
        /// Read-only ticket (can sync and download, never write). Use this for seeders.
        #[arg(long)]
        read_only: bool,
        /// Leave this machine's IP addresses out of the ticket
        #[arg(long)]
        hide_ip: bool,
    },
    /// Join a collaborator's session using an invite ticket
    Join {
        ticket: String,
        /// Display name (defaults to your saved profile)
        #[arg(long)]
        author: Option<String>,
        /// Seconds to stay connected for initial sync (also downloads the newest snapshot's files)
        #[arg(long, default_value_t = 10)]
        wait_secs: u64,
        #[arg(long)]
        json: bool,
    },
    /// Push a deduplicated snapshot of an FL Studio project (.flp or .zip)
    Push {
        session_id: String,
        /// Path to .flp or zipped loop .zip
        path: PathBuf,
        /// Commit note describing your musical changes
        #[arg(long, default_value = "")]
        note: String,
        /// Display name (defaults to your saved profile)
        #[arg(long)]
        author: Option<String>,
        /// Snapshot this version is based on (defaults to the folder's .flint record, then the newest)
        #[arg(long)]
        parent: Option<String>,
        /// Optional seconds to keep serving peers after pushing (useful for CLI tests)
        #[arg(long, default_value_t = 0)]
        serve_secs: u64,
        #[arg(long)]
        json: bool,
    },
    /// Pull and materialize a snapshot into a new local folder
    Pull {
        session_id: String,
        /// Destination root directory
        #[arg(long)]
        out: PathBuf,
        /// Specific snapshot ID (defaults to latest)
        #[arg(long)]
        snapshot: Option<String>,
        /// Seconds to keep trying to sync the snapshot and download its files
        #[arg(long, default_value_t = 30)]
        wait_secs: u64,
        /// Do not rewrite sample paths inside the pulled .flp
        #[arg(long)]
        no_rewrite: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show chronological version history and musical diffs for a session
    Log {
        session_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Download (and keep) the files of the newest — or all — snapshots
    Sync {
        session_id: String,
        /// Fetch every snapshot, not just the newest
        #[arg(long)]
        all: bool,
        #[arg(long, default_value_t = 30)]
        wait_secs: u64,
        #[arg(long)]
        json: bool,
    },
    /// Free disk space used by files that only older snapshots need (this machine only)
    Prune {
        session_id: String,
        /// Number of newest snapshots whose files are kept
        #[arg(long, default_value_t = 10)]
        keep: usize,
        /// Only report what would be released
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Move this session's history into a brand-new session (use to remove a collaborator)
    Rotate {
        session_id: String,
        /// Display name (defaults to your saved profile)
        #[arg(long)]
        author: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = resolve_data_dir(cli.data_dir);

    match cli.command {
        Some(Commands::Inspect { path, json }) => cmd_inspect(path, json),
        Some(Commands::Diff { old, new, json }) => cmd_diff(old, new, json),
        Some(Commands::ScanPlugins { extra_dirs, json }) => cmd_scan_plugins(extra_dirs, json),
        Some(Commands::Check {
            path,
            peer_name,
            peer_plugins,
            peer_fl_version,
            json,
        }) => cmd_check(path, peer_name, peer_plugins, peer_fl_version, json),
        Some(Commands::Profile { action }) => cmd_profile(&data_dir, action),
        Some(Commands::Session { action }) => cmd_session(data_dir, action).await,
        Some(Commands::Seeder {
            add_tickets,
            allow_write,
            prefetch_interval_secs,
        }) => cmd_seeder(data_dir, add_tickets, allow_write, prefetch_interval_secs).await,
        None => {
            println!(
                "flint-daemon {} — run with --help for commands (inspect, diff, scan-plugins, check, profile, session, seeder).",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
    }
}

fn resolve_data_dir(override_dir: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = override_dir {
        return dir;
    }
    if let Some(proj) = ProjectDirs::from("dev", "FLint", "FLint") {
        return proj.data_local_dir().to_path_buf();
    }
    PathBuf::from(".flint-data")
}

// -----------------------------------------------------------------------------
// Profile (persistent display name)
// -----------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct Profile {
    display_name: String,
}

fn profile_path(data_dir: &Path) -> PathBuf {
    data_dir.join("profile.json")
}

fn load_profile(data_dir: &Path) -> Option<Profile> {
    let raw = std::fs::read(profile_path(data_dir)).ok()?;
    serde_json::from_slice::<Profile>(&raw)
        .ok()
        .filter(|p| !p.display_name.trim().is_empty())
}

/// `--author` flag > saved profile > OS user name > "Producer".
fn resolve_author(data_dir: &Path, flag: Option<String>) -> String {
    if let Some(name) = flag.filter(|n| !n.trim().is_empty()) {
        return name.trim().to_string();
    }
    if let Some(p) = load_profile(data_dir) {
        return p.display_name;
    }
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Producer".to_string())
}

fn cmd_profile(data_dir: &Path, action: ProfileAction) -> Result<()> {
    match action {
        ProfileAction::Show => {
            match load_profile(data_dir) {
                Some(p) => println!("Display name: {}", p.display_name),
                None => println!(
                    "No profile saved. Using '{}'. Set one with: flint-daemon profile set --name \"Your Name\"",
                    resolve_author(data_dir, None)
                ),
            }
            Ok(())
        }
        ProfileAction::Set { name } => {
            let name = name.trim().to_string();
            anyhow::ensure!(!name.is_empty(), "display name cannot be empty");
            std::fs::create_dir_all(data_dir)?;
            let profile = Profile { display_name: name };
            std::fs::write(profile_path(data_dir), serde_json::to_vec_pretty(&profile)?)?;
            println!("Saved display name: {}", profile.display_name);
            Ok(())
        }
    }
}

// -----------------------------------------------------------------------------
// Sessions
// -----------------------------------------------------------------------------

async fn cmd_session(data_dir: PathBuf, action: SessionAction) -> Result<()> {
    // Prune needs a garbage-collection run before the process exits to actually free space.
    let options = match &action {
        SessionAction::Prune { dry_run: false, .. } => NodeOptions {
            gc_interval: Duration::from_secs(2),
            ..Default::default()
        },
        _ => NodeOptions::default(),
    };
    let node = SyncNode::start_with_options(&data_dir, options).await?;
    node.wait_online(Duration::from_secs(4)).await;

    let res = run_session_action(&node, &data_dir, action).await;

    node.shutdown().await?;
    res
}

async fn run_session_action(node: &SyncNode, data_dir: &Path, action: SessionAction) -> Result<()> {
    match action {
        SessionAction::Create { name, author, json } => {
            let author = resolve_author(data_dir, author);
            let created = node.create_session(&name, &author).await?;
            let env = build_local_peer_env(node, &author);
            node.publish_peer_env(&created.session_id, &env).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&created)?);
            } else {
                println!("Session created: {}", created.meta.name);
                println!("Session ID:      {}", created.session_id);
                println!("Node ID:         {}", node.node_id());
                println!("Invite Ticket (gives WRITE access — share only with collaborators):");
                println!("{}", created.invite_ticket);
                println!();
                println!("Keep the session ID private too: anyone who has it can read the session.");
            }
            Ok(())
        }
        SessionAction::Share {
            session_id,
            read_only,
            hide_ip,
        } => {
            let ticket = node
                .share_session(
                    &session_id,
                    ShareOptions {
                        read_only,
                        hide_addresses: hide_ip,
                    },
                )
                .await?;
            println!("{ticket}");
            Ok(())
        }
        SessionAction::Join {
            ticket,
            author,
            wait_secs,
            json,
        } => {
            let author = resolve_author(data_dir, author);
            let sid = node.join_session(&ticket).await?;
            let env = build_local_peer_env(node, &author);
            node.publish_peer_env(&sid, &env).await?;
            let mut prefetched = None;
            if wait_secs > 0 {
                node.wait_for_sync_ready(&sid, Duration::from_secs(wait_secs))
                    .await?;
                // Grab the newest version's files while the inviter is online.
                prefetched = Some(
                    node.prefetch(&sid, PrefetchScope::Latest, Duration::from_secs(wait_secs))
                        .await?,
                );
            }
            let snapshots = node.list_snapshots(&sid).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session_id": sid,
                        "node_id": node.node_id(),
                        "snapshots_synced": snapshots.len(),
                        "prefetch": prefetched,
                    }))?
                );
            } else {
                println!("Joined session:   {sid}");
                println!("Snapshots synced: {}", snapshots.len());
                if let Some(p) = prefetched {
                    println!(
                        "Newest version:   {} file(s) local, {} downloaded, {} unavailable",
                        p.already_local,
                        p.downloaded,
                        p.unavailable.len()
                    );
                }
            }
            Ok(())
        }
        SessionAction::Push {
            session_id,
            path,
            note,
            author,
            parent,
            serve_secs,
            json,
        } => {
            let pushed = node
                .push_project(
                    &session_id,
                    &path,
                    PushOptions {
                        note,
                        author_display_name: resolve_author(data_dir, author),
                        parent,
                        ..Default::default()
                    },
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pushed)?);
            } else {
                println!("Pushed snapshot: {}", pushed.snapshot.id);
                let parent_line = match (pushed.parent_source, pushed.snapshot.parent_ids.first()) {
                    (ParentSource::None, _) | (_, None) => "none (first version)".to_string(),
                    (ParentSource::Explicit, Some(p)) => format!("{p} (from --parent)"),
                    (ParentSource::Workspace, Some(p)) => format!("{p} (this folder's .flint record)"),
                    (ParentSource::Latest, Some(p)) => {
                        format!("{p} (assumed newest — no .flint record in this folder)")
                    }
                };
                println!("Based on:        {parent_line}");
                println!(
                    "Deduplication:   {} new blob(s) ({} bytes), {} reused blob(s)",
                    pushed.new_blobs, pushed.total_bytes_ingested, pushed.reused_blobs
                );
                if pushed.diverged_from_latest {
                    println!(
                        "Note:            someone pushed {} after your base version — this push is a parallel branch.",
                        pushed.latest_snapshot_id.as_deref().unwrap_or("?")
                    );
                }
                if !pushed.missing_samples.is_empty() {
                    println!(
                        "⚠ {} sample(s) were NOT found on disk and are not included — collaborators will see them as missing:",
                        pushed.missing_samples.len()
                    );
                    for s in &pushed.missing_samples {
                        println!("  - {s}");
                    }
                }
                if !pushed.snapshot.diff_summary.is_empty() {
                    println!("Musical diff:");
                    for line in &pushed.snapshot.diff_summary {
                        println!("  • {line}");
                    }
                }
            }
            if serve_secs > 0 {
                eprintln!("Serving peers for {serve_secs}s...");
                tokio::time::sleep(Duration::from_secs(serve_secs)).await;
            }
            Ok(())
        }
        SessionAction::Pull {
            session_id,
            out,
            snapshot,
            wait_secs,
            no_rewrite,
            json,
        } => {
            let pulled = node
                .pull_snapshot(
                    &session_id,
                    &out,
                    PullOptions {
                        snapshot_id: snapshot,
                        wait: Duration::from_secs(wait_secs),
                        rewrite_sample_paths: !no_rewrite,
                    },
                )
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pulled)?);
            } else {
                println!("Pulled snapshot: {}", pulled.snapshot.id);
                println!("Author / Note:   {} — {}", pulled.snapshot.author, pulled.snapshot.note);
                println!("FLP Path:        {}", pulled.flp_path.display());
                println!(
                    "Files written:   {} ({} downloaded from peers)",
                    pulled.files_written, pulled.downloaded_blobs
                );
                if pulled.rewritten_sample_paths > 0 {
                    println!(
                        "Sample paths:    {} rewritten to the pulled files (original kept at {})",
                        pulled.rewritten_sample_paths,
                        pulled
                            .original_flp_path
                            .as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    );
                }
                if !pulled.snapshot.missing_samples.is_empty() {
                    println!(
                        "⚠ The author was missing {} sample(s) when pushing:",
                        pulled.snapshot.missing_samples.len()
                    );
                    for s in &pulled.snapshot.missing_samples {
                        println!("  - {s}");
                    }
                }
            }
            Ok(())
        }
        SessionAction::Log { session_id, json } => {
            let snapshots = node.list_snapshots(&session_id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&snapshots)?);
            } else if snapshots.is_empty() {
                println!("No snapshots in session {session_id}.");
            } else {
                println!("Snapshots ({}):", snapshots.len());
                for (idx, s) in snapshots.iter().enumerate() {
                    let parent = s
                        .parent_ids
                        .first()
                        .map(|p| format!(" ← {p}"))
                        .unwrap_or_default();
                    println!(
                        "  #{} [{}{}] by {} — {} (BPM {:.1})",
                        idx + 1,
                        s.id,
                        parent,
                        s.author,
                        if s.note.is_empty() { "(no note)" } else { &s.note },
                        s.manifest.bpm
                    );
                    for line in &s.diff_summary {
                        println!("      • {line}");
                    }
                    if !s.missing_samples.is_empty() {
                        println!("      ⚠ {} missing sample(s)", s.missing_samples.len());
                    }
                }
            }
            Ok(())
        }
        SessionAction::Sync {
            session_id,
            all,
            wait_secs,
            json,
        } => {
            node.wait_for_sync_ready(&session_id, Duration::from_secs(wait_secs.min(10)))
                .await?;
            let scope = if all {
                PrefetchScope::All
            } else {
                PrefetchScope::Latest
            };
            let report = node
                .prefetch(&session_id, scope, Duration::from_secs(wait_secs))
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Files: {} requested, {} already local, {} downloaded, {} unavailable",
                    report.requested,
                    report.already_local,
                    report.downloaded,
                    report.unavailable.len()
                );
            }
            Ok(())
        }
        SessionAction::Prune {
            session_id,
            keep,
            dry_run,
            json,
        } => {
            let report = node.prune(&session_id, keep, dry_run).await?;
            if !dry_run && report.released_blobs > 0 {
                // Let at least one garbage-collection pass run (interval is 2s for this command).
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "{} the files of {} older snapshot(s); keeping the newest {}.",
                    if dry_run { "Would release" } else { "Released" },
                    report.pruned_snapshots,
                    report.kept_snapshots
                );
                println!("Files released: {}", report.released_blobs);
                if report.legacy_pinned_blobs > 0 {
                    println!(
                        "{} file(s) are pinned by this session's pre-v0.2.1 registry and cannot be pruned \
                         (rotate the session to start clean).",
                        report.legacy_pinned_blobs
                    );
                }
            }
            Ok(())
        }
        SessionAction::Rotate {
            session_id,
            author,
            json,
        } => {
            let author = resolve_author(data_dir, author);
            let rotated = node.rotate_session(&session_id, &author).await?;
            let env = build_local_peer_env(node, &author);
            node.publish_peer_env(&rotated.created.session_id, &env).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rotated)?);
            } else {
                println!("Old session:     {} (no longer synced here)", rotated.old_session_id);
                println!("New session ID:  {}", rotated.created.session_id);
                println!("Entries copied:  {}", rotated.copied_entries);
                println!("New invite ticket — send it ONLY to people who should keep access:");
                println!("{}", rotated.created.invite_ticket);
                println!();
                println!(
                    "Everyone still in the old session keeps what was there before; they cannot see anything new."
                );
            }
            Ok(())
        }
    }
}

async fn cmd_seeder(
    data_dir: PathBuf,
    add_tickets: Vec<String>,
    allow_write: bool,
    prefetch_interval_secs: u64,
) -> Result<()> {
    let node = SyncNode::start(&data_dir).await?;
    node.wait_online(Duration::from_secs(5)).await;

    println!("FLint P2P Seeder online");
    println!("Node ID:  {}", node.node_id());
    println!("Data Dir: {}", data_dir.display());

    for ticket in &add_tickets {
        let sid = node.add_seeder_ticket(ticket, allow_write).await?;
        println!(
            "Pinned new session: {sid} ({})",
            if allow_write { "write access kept" } else { "read-only" }
        );
    }

    let mut sessions = Vec::new();
    for ticket in &node.load_seeder_tickets()? {
        match node.join_session(ticket).await {
            Ok(sid) => {
                println!("Seeding session:    {sid}");
                sessions.push(sid);
            }
            Err(err) => eprintln!("Warning: failed to rejoin pinned ticket: {err}"),
        }
    }

    println!(
        "Seeding {} session(s), fetching new files every {prefetch_interval_secs}s. Press Ctrl+C to stop.",
        sessions.len()
    );

    // Metadata syncs automatically; files are downloaded here so the seeder can serve them
    // while their authors are offline.
    let interval = Duration::from_secs(prefetch_interval_secs.max(5));
    loop {
        for sid in &sessions {
            match node.prefetch(sid, PrefetchScope::All, interval).await {
                Ok(r) if r.downloaded > 0 || !r.unavailable.is_empty() => println!(
                    "[{sid}] downloaded {} file(s), {} not yet available",
                    r.downloaded,
                    r.unavailable.len()
                ),
                Ok(_) => {}
                Err(err) => eprintln!("[{sid}] prefetch failed: {err}"),
            }
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }

    println!("\nShutting down seeder...");
    node.shutdown().await?;
    Ok(())
}

fn build_local_peer_env(node: &SyncNode, display_name: &str) -> PeerEnvironment {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    PeerEnvironment {
        node_id: node.node_id(),
        display_name: display_name.to_string(),
        fl_version: None,
        installed_plugins: flint_vst_scan::scan_installed_plugins(),
        updated_at: now,
    }
}

fn cmd_inspect(path: PathBuf, json: bool) -> Result<()> {
    let manifest = flint_flp::parse_flp_path(&path)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
        return Ok(());
    }

    println!("Project: {}", path.display());
    if !manifest.title.is_empty() {
        println!("Title:   {}", manifest.title);
    }
    println!(
        "FL Studio: {} | BPM: {:.2} | Time Sig: {}/{} | PPQ: {}",
        manifest.fl_version, manifest.bpm, manifest.time_sig_num, manifest.time_sig_den, manifest.ppq
    );
    if let Some(dp) = &manifest.data_path {
        println!("Data Dir:  {dp}");
    }

    println!("\nChannels ({}):", manifest.channels.len());
    for ch in &manifest.channels {
        let plugin = ch
            .effective_plugin_name()
            .map(|p| format!(" [{p}]"))
            .unwrap_or_default();
        let sample = ch
            .sample_path
            .as_deref()
            .map(|s| format!(" -> {}", s.rsplit(['\\', '/']).next().unwrap_or(s)))
            .unwrap_or_default();
        println!("  {:>2}. {}{}{}", ch.index + 1, ch.name, plugin, sample);
    }

    println!("\nActive Mixer Inserts ({}):", manifest.mixer_inserts.len());
    for ins in &manifest.mixer_inserts {
        let fx = if ins.fx_plugins.is_empty() {
            "(no FX)".to_string()
        } else {
            ins.fx_plugins.join(" -> ")
        };
        let label = if ins.index == 0 {
            format!("Master ('{}')", ins.name)
        } else if ins.name == format!("Insert {}", ins.index) {
            ins.name.clone()
        } else {
            format!("Insert {} ('{}')", ins.index, ins.name)
        };
        println!("  {label} -> {fx}");
    }

    println!("\nPatterns ({}):", manifest.patterns.len());
    for pat in &manifest.patterns {
        println!(
            "  {:>2}. {} ({} notes, hash {})",
            pat.index, pat.name, pat.note_count, pat.notes_hash
        );
    }

    if !manifest.arrangements.is_empty() {
        println!("\nArrangements ({}):", manifest.arrangements.len());
        for arr in &manifest.arrangements {
            println!(
                "  {:>2}. {} ({} bytes of playlist data)",
                arr.index + 1,
                arr.name,
                arr.playlist_bytes
            );
        }
    }

    println!("\nSamples ({}):", manifest.samples.len());
    for sample in &manifest.samples {
        println!("  - {sample}");
    }

    Ok(())
}

fn cmd_diff(old: PathBuf, new: PathBuf, json: bool) -> Result<()> {
    let old_manifest = flint_flp::parse_flp_path(&old)
        .with_context(|| format!("failed to parse {}", old.display()))?;
    let new_manifest = flint_flp::parse_flp_path(&new)
        .with_context(|| format!("failed to parse {}", new.display()))?;
    let diff = flint_flp::diff_manifests(&old_manifest, &new_manifest);

    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
        return Ok(());
    }

    if diff.is_empty() {
        println!("No musical changes detected.");
    } else {
        println!("Musical Diff ({} change(s)):", diff.summary_lines.len());
        for line in &diff.summary_lines {
            println!("  • {line}");
        }
    }
    Ok(())
}

fn cmd_scan_plugins(extra_dirs: Vec<PathBuf>, json: bool) -> Result<()> {
    let plugins = flint_vst_scan::scan_installed_plugins_with_extras(&extra_dirs);
    if json {
        println!("{}", serde_json::to_string_pretty(&plugins)?);
        return Ok(());
    }
    println!("Installed third-party plugins ({}):", plugins.len());
    for p in &plugins {
        println!("  - {p}");
    }
    Ok(())
}

fn cmd_check(
    path: PathBuf,
    peer_name: String,
    peer_plugins: Option<String>,
    peer_fl_version: Option<String>,
    json: bool,
) -> Result<()> {
    let manifest = flint_flp::parse_flp_path(&path)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let installed_plugins = match peer_plugins {
        Some(csv) => csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        None => flint_vst_scan::scan_installed_plugins(),
    };

    let peer = PeerEnvironment {
        node_id: "local".into(),
        display_name: peer_name,
        fl_version: peer_fl_version,
        installed_plugins,
        updated_at: 0,
    };

    let report = flint_vst_scan::check_compatibility(&manifest, &peer);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if report.is_compatible() {
        println!(
            "✓ Compatible with {} (no missing plugins or FL version conflicts).",
            report.peer_display_name
        );
    } else {
        println!("⚠ Compatibility warnings for {}:", report.peer_display_name);
        if let Some(mismatch) = &report.fl_version_mismatch {
            println!("  • {mismatch}");
        }
        for w in &report.missing_plugins {
            println!("  • Missing '{}' at {}", w.plugin_name, w.location);
        }
    }
    Ok(())
}
