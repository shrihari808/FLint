//! Persistent local daemon + CLI + optional `--seeder` entrypoint for FLint.

use anyhow::Result;
use flint_core::ProjectManifest;
use flint_sync::SyncEngine;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("flint-daemon {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let _engine = SyncEngine::new()?;
    let _empty = ProjectManifest::default();
    let _plugins = flint_vst_scan::scan_installed_plugins();
    let _diff = flint_flp::diff_manifests(&_empty, &_empty);

    println!(
        "flint-daemon {} workspace initialized (Phase 0). Run with --version for version info.",
        env!("CARGO_PKG_VERSION")
    );
    Ok(())
}
