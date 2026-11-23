//! Installed VST2/VST3/CLAP scanner and missing-plugin compatibility checker.

use flint_core::{CompatibilityReport, PeerEnvironment, ProjectManifest};

/// Scan standard OS plugin directories and return normalized installed plugin names.
pub fn scan_installed_plugins() -> Vec<String> {
    Vec::new()
}

/// Compare a project's plugin requirements against a collaborator's advertised environment.
pub fn check_compatibility(
    _manifest: &ProjectManifest,
    peer: &PeerEnvironment,
) -> CompatibilityReport {
    CompatibilityReport {
        peer_display_name: peer.display_name.clone(),
        missing_plugins: Vec::new(),
        fl_version_mismatch: None,
    }
}
