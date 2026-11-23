//! Persistent local daemon + CLI + optional `--seeder` entrypoint for FLint.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use flint_core::PeerEnvironment;
use flint_sync::SyncNode;
use std::path::PathBuf;
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
    },
}

#[derive(Subcommand, Debug)]
enum SessionAction {
    /// Create a new P2P session and print its shareable invite ticket
    Create {
        /// Human-friendly session / track name
        name: String,
        /// Your display name
        #[arg(long, default_value = "Producer")]
        author: String,
        #[arg(long)]
        json: bool,
    },
    /// Print a fresh invite ticket for an existing session
    Share {
        session_id: String,
    },
    /// Join a collaborator's session using an invite ticket
    Join {
        ticket: String,
        /// Your display name (published with your installed VST inventory)
        #[arg(long, default_value = "Collaborator")]
        author: String,
        /// Seconds to stay connected for initial sync
        #[arg(long, default_value_t = 5)]
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
        /// Your display name
        #[arg(long, default_value = "Producer")]
        author: String,
        /// Optional seconds to keep serving peers after pushing (useful for CLI tests)
        #[arg(long, default_value_t = 0)]
        serve_secs: u64,
        #[arg(long)]
        json: bool,
    },
    /// Pull and materialize a snapshot into a clean local folder
    Pull {
        session_id: String,
        /// Destination root directory
        #[arg(long)]
        out: PathBuf,
        /// Specific snapshot ID (defaults to latest)
        #[arg(long)]
        snapshot: Option<String>,
        /// Seconds to wait for P2P sync before pulling
        #[arg(long, default_value_t = 5)]
        wait_secs: u64,
        #[arg(long)]
        json: bool,
    },
    /// Show chronological version history and musical diffs for a session
    Log {
        session_id: String,
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
        Some(Commands::Session { action }) => cmd_session(data_dir, action).await,
        Some(Commands::Seeder { add_tickets }) => cmd_seeder(data_dir, add_tickets).await,
        None => {
            println!(
                "flint-daemon {} — run with --help for commands (inspect, diff, scan-plugins, check, session, seeder).",
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

async fn cmd_session(data_dir: PathBuf, action: SessionAction) -> Result<()> {
    let node = SyncNode::start(&data_dir).await?;
    node.wait_online(Duration::from_secs(4)).await;

    let res = match action {
        SessionAction::Create { name, author, json } => {
            let created = node.create_session(&name, &author).await?;
            let env = build_local_peer_env(&node, &author);
            node.publish_peer_env(&created.session_id, &env).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&created)?);
            } else {
                println!("Session created: {}", created.meta.name);
                println!("Session ID:      {}", created.session_id);
                println!("Node ID:         {}", node.node_id());
                println!("Invite Ticket:\n{}", created.invite_ticket);
            }
            Ok(())
        }
        SessionAction::Share { session_id } => {
            let ticket = node.share_session(&session_id).await?;
            println!("{ticket}");
            Ok(())
        }
        SessionAction::Join {
            ticket,
            author,
            wait_secs,
            json,
        } => {
            let sid = node.join_session(&ticket).await?;
            let env = build_local_peer_env(&node, &author);
            node.publish_peer_env(&sid, &env).await?;
            if wait_secs > 0 {
                node.wait_for_sync_ready(&sid, Duration::from_secs(wait_secs))
                    .await?;
            }
            let snapshots = node.list_snapshots(&sid).await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session_id": sid,
                        "node_id": node.node_id(),
                        "snapshots_synced": snapshots.len()
                    }))?
                );
            } else {
                println!("Joined session:   {sid}");
                println!("Snapshots synced: {}", snapshots.len());
            }
            Ok(())
        }
        SessionAction::Push {
            session_id,
            path,
            note,
            author,
            serve_secs,
            json,
        } => {
            let pushed = node
                .push_project(&session_id, &path, &note, &author, None, Vec::new())
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pushed)?);
            } else {
                println!("Pushed snapshot: {}", pushed.snapshot.id);
                println!(
                    "Deduplication:   {} new blob(s) ({} bytes), {} reused blob(s)",
                    pushed.new_blobs, pushed.total_bytes_ingested, pushed.reused_blobs
                );
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
            json,
        } => {
            if wait_secs > 0 {
                node.wait_for_sync_ready(&session_id, Duration::from_secs(wait_secs))
                    .await?;
            }
            let pulled = node
                .pull_snapshot(&session_id, snapshot.as_deref(), &out)
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&pulled)?);
            } else {
                println!("Pulled snapshot: {}", pulled.snapshot.id);
                println!("Author / Note:   {} — {}", pulled.snapshot.author, pulled.snapshot.note);
                println!("FLP Path:        {}", pulled.flp_path.display());
                println!("Files written:   {}", pulled.files_written);
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
                    println!(
                        "  #{} [{}] by {} — {} (BPM {:.1})",
                        idx + 1,
                        s.id,
                        s.author,
                        if s.note.is_empty() { "(no note)" } else { &s.note },
                        s.manifest.bpm
                    );
                    for line in &s.diff_summary {
                        println!("      • {line}");
                    }
                }
            }
            Ok(())
        }
    };

    node.shutdown().await?;
    res
}

async fn cmd_seeder(data_dir: PathBuf, add_tickets: Vec<String>) -> Result<()> {
    let node = SyncNode::start(&data_dir).await?;
    node.wait_online(Duration::from_secs(5)).await;

    println!("FLint P2P Seeder online");
    println!("Node ID:  {}", node.node_id());
    println!("Data Dir: {}", data_dir.display());

    for ticket in &add_tickets {
        let sid = node.add_seeder_ticket(ticket).await?;
        println!("Pinned new session: {sid}");
    }

    let pinned = node.load_seeder_tickets()?;
    for ticket in &pinned {
        match node.join_session(ticket).await {
            Ok(sid) => println!("Seeding session:    {sid}"),
            Err(err) => eprintln!("Warning: failed to rejoin pinned ticket: {err}"),
        }
    }

    println!("Seeding {} session(s). Press Ctrl+C to stop.", pinned.len());
    tokio::signal::ctrl_c().await?;
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
