//! Persistent local daemon + CLI + optional `--seeder` entrypoint for FLint.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use flint_core::PeerEnvironment;
use flint_sync::SyncEngine;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "flint-daemon",
    version,
    about = "FLint — DAW-aware P2P collaboration daemon & FL Studio project analyzer"
)]
struct Cli {
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

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
        None => {
            let _engine = SyncEngine::new()?;
            println!(
                "flint-daemon {} — run with --help to see available commands (inspect, diff, scan-plugins, check).",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
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
        println!("✓ Compatible with {} (no missing plugins or FL version conflicts).", report.peer_display_name);
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
