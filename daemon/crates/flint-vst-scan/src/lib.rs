//! Installed VST2/VST3/CLAP scanner and missing-plugin compatibility checker.

use flint_core::{
    CompatibilityReport, MissingPluginWarning, PeerEnvironment, ProjectManifest,
};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// Known FL Studio stock generators & effects (never flagged as missing 3rd-party VSTs).
const FL_STOCK_PLUGINS: &[&str] = &[
    "3x osc",
    "audio clip",
    "autoclips",
    "automan",
    "bassdrum",
    "beepslice",
    "boobass",
    "control surface",
    "convolver",
    "directwave",
    "distructor",
    "drumpad",
    "drumaxx",
    "edison",
    "effector",
    "emphasis",
    "equo",
    "fl keys",
    "fl slayer",
    "flex",
    "fpc",
    "frequency splitter",
    "fruit kick",
    "gms",
    "gross beat",
    "hardcore",
    "harmless",
    "harmor",
    "hyper chorus",
    "kepler",
    "kepler exo",
    "layer",
    "low lifter",
    "luxeverb",
    "maximus",
    "midi out",
    "minisynth",
    "morphine",
    "multiband delay",
    "newtime",
    "newtone",
    "ogun",
    "patcher",
    "pitch shifter",
    "plucked",
    "poizone",
    "sakura",
    "sampler",
    "sawer",
    "simsynth",
    "slicex",
    "soundfont player",
    "soundgoodizer",
    "spreader",
    "sytrus",
    "toxic biohazard",
    "transient processor",
    "transistor bass",
    "tuner",
    "vintage chorus",
    "vintage phaser",
    "vocodex",
    "wasp",
    "wasp xt",
    "wave candy",
    "zgameeditor visualizer",
];

/// Returns default OS-standard plugin search directories.
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        vec![
            PathBuf::from(r"C:\Program Files\Common Files\VST3"),
            PathBuf::from(r"C:\Program Files\Common Files\CLAP"),
            PathBuf::from(r"C:\Program Files\Common Files\VST2"),
            PathBuf::from(r"C:\Program Files\VSTPlugins"),
            PathBuf::from(r"C:\Program Files\Steinberg\VSTPlugins"),
            PathBuf::from(r"C:\Program Files (x86)\Common Files\VST3"),
            PathBuf::from(r"C:\Program Files (x86)\VSTPlugins"),
        ]
    }
    #[cfg(target_os = "macos")]
    {
        let mut dirs = vec![
            PathBuf::from("/Library/Audio/Plug-Ins/VST3"),
            PathBuf::from("/Library/Audio/Plug-Ins/VST"),
            PathBuf::from("/Library/Audio/Plug-Ins/CLAP"),
            PathBuf::from("/Library/Audio/Plug-Ins/Components"),
        ];
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(&home).join("Library/Audio/Plug-Ins/VST3"));
            dirs.push(PathBuf::from(&home).join("Library/Audio/Plug-Ins/CLAP"));
        }
        dirs
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        vec![
            PathBuf::from("/usr/lib/vst3"),
            PathBuf::from("/usr/local/lib/vst3"),
            PathBuf::from("/usr/lib/clap"),
        ]
    }
}

/// Per-user plugin folders (no admin rights needed to install into these), plus any folders
/// listed in the `FLINT_PLUGIN_DIRS` environment variable (`;`-separated on Windows,
/// `:`-separated elsewhere) — use that for custom folders configured in FL Studio's
/// *Manage plugins → Plugin search paths*.
pub fn user_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(target_os = "windows")]
    {
        // VST3 spec per-user location, and the CLAP equivalent.
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let common = PathBuf::from(local).join("Programs").join("Common");
            dirs.push(common.join("VST3"));
            dirs.push(common.join("CLAP"));
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(&home).join(".vst3"));
            dirs.push(PathBuf::from(&home).join(".clap"));
            dirs.push(PathBuf::from(&home).join(".vst"));
        }
    }
    if let Some(extra) = std::env::var_os("FLINT_PLUGIN_DIRS") {
        dirs.extend(std::env::split_paths(&extra).filter(|p| !p.as_os_str().is_empty()));
    }
    dirs
}

/// FL Studio's own plugin database folders
/// (`Documents/Image-Line/<FL Studio folder>/Presets/Plugin database/Installed`).
///
/// FL Studio writes one `.nfo`/`.fst` file per plugin it has found, named the way FL displays
/// the plugin. That matches the names stored in `.flp` files far better than guessing from
/// `.vst3`/`.dll` filenames, and it includes plugins in custom search folders.
pub fn fl_plugin_database_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(user_dirs) = directories::UserDirs::new() else {
        return out;
    };
    let Some(docs) = user_dirs.document_dir() else {
        return out;
    };
    let image_line = docs.join("Image-Line");
    let Ok(entries) = std::fs::read_dir(&image_line) else {
        return out;
    };
    for entry in entries.flatten() {
        let installed = entry
            .path()
            .join("Presets")
            .join("Plugin database")
            .join("Installed");
        if installed.is_dir() {
            out.push(installed);
        }
    }
    out
}

/// Collect plugin names from an FL Studio plugin database folder.
pub fn scan_fl_plugin_database(dir: &Path) -> Vec<String> {
    let mut found = BTreeSet::new();
    walk_fl_database(dir, 0, &mut found);
    found.into_iter().collect()
}

fn walk_fl_database(dir: &Path, depth: usize, found: &mut BTreeSet<String>) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_fl_database(&path, depth + 1, found);
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if matches!(ext.as_deref(), Some("nfo") | Some("fst")) {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let cleaned = clean_display_name(stem);
                if !cleaned.is_empty() {
                    found.insert(cleaned);
                }
            }
        }
    }
}

/// Scan default OS plugin directories, per-user folders, and FL Studio's plugin database,
/// and return a sorted, deduplicated list of plugin names.
pub fn scan_installed_plugins() -> Vec<String> {
    scan_installed_plugins_with_extras(&[])
}

/// Same as [`scan_installed_plugins`], plus any extra folders.
pub fn scan_installed_plugins_with_extras(extra_dirs: &[PathBuf]) -> Vec<String> {
    let mut dirs = default_plugin_dirs();
    dirs.extend(user_plugin_dirs());
    dirs.extend(extra_dirs.iter().cloned());
    let mut found: BTreeSet<String> = scan_plugin_dirs(&dirs).into_iter().collect();
    for db in fl_plugin_database_dirs() {
        found.extend(scan_fl_plugin_database(&db));
    }
    found.into_iter().collect()
}

/// Scan a specific set of directories for `.vst3`, `.clap`, and `.dll`/`.vst`/`.component` plugins.
pub fn scan_plugin_dirs(dirs: &[PathBuf]) -> Vec<String> {
    let mut found = BTreeSet::new();
    for dir in dirs {
        if dir.exists() {
            walk_plugin_dir(dir, 0, &mut found);
        }
    }
    found.into_iter().collect()
}

fn walk_plugin_dir(dir: &Path, depth: usize, found: &mut BTreeSet<String>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());

        match ext.as_deref() {
            // VST3 and AU components are often directory bundles — record and do NOT recurse inside them.
            Some("vst3") | Some("clap") | Some("component") | Some("vst") => {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let cleaned = clean_display_name(stem);
                    if !cleaned.is_empty() {
                        found.insert(cleaned);
                    }
                }
            }
            Some("dll") if path.is_file() => {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let cleaned = clean_display_name(stem);
                    if !cleaned.is_empty() {
                        found.insert(cleaned);
                    }
                }
            }
            _ => {
                if path.is_dir() {
                    walk_plugin_dir(&path, depth + 1, found);
                }
            }
        }
    }
}

/// Returns `true` if `plugin_name` is a built-in FL Studio stock plugin.
pub fn is_fl_stock_plugin(plugin_name: &str) -> bool {
    let lower = plugin_name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return true;
    }
    if lower.starts_with("fruity ") && lower != "fruity wrapper" {
        return true;
    }
    FL_STOCK_PLUGINS.contains(&lower.as_str())
}

/// Normalize a plugin name for fuzzy cross-machine matching
/// (e.g. `"Serum 2 (x64)"` and `"Serum2.vst3"` both normalize to `"serum2"`).
pub fn normalize_plugin_key(name: &str) -> String {
    let stripped = clean_display_name(name).to_ascii_lowercase();
    stripped
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn clean_display_name(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    for suffix in [
        ".vst3", ".vst", ".dll", ".clap", " (x64)", " (64-bit)", " (x86)", "_x64", "_64bit",
        " x64",
    ] {
        if s.to_ascii_lowercase().ends_with(suffix) {
            let new_len = s.len().saturating_sub(suffix.len());
            s.truncate(new_len);
            s = s.trim().to_string();
        }
    }
    s
}

/// Compare a project's plugin & FL version requirements against a collaborator's advertised environment.
pub fn check_compatibility(
    manifest: &ProjectManifest,
    peer: &PeerEnvironment,
) -> CompatibilityReport {
    let installed_keys: HashSet<String> = peer
        .installed_plugins
        .iter()
        .map(|p| normalize_plugin_key(p))
        .collect();

    let mut missing = Vec::new();
    let mut seen_warning = HashSet::new();

    // Check Channel Rack generator plugins
    for ch in &manifest.channels {
        if let Some(plugin) = ch.effective_plugin_name() {
            if is_fl_stock_plugin(plugin) || plugin.eq_ignore_ascii_case("Fruity Wrapper") {
                continue;
            }
            let key = normalize_plugin_key(plugin);
            if !installed_keys.contains(&key) {
                let loc = format!("Channel {} ('{}')", ch.index + 1, ch.name);
                if seen_warning.insert((key, loc.clone())) {
                    missing.push(MissingPluginWarning {
                        plugin_name: plugin.to_string(),
                        location: loc,
                    });
                }
            }
        }
    }

    // Check Mixer Insert FX plugins
    for ins in &manifest.mixer_inserts {
        for (slot_idx, fx) in ins.fx_plugins.iter().enumerate() {
            if is_fl_stock_plugin(fx) || fx.eq_ignore_ascii_case("Fruity Wrapper") {
                continue;
            }
            let key = normalize_plugin_key(fx);
            if !installed_keys.contains(&key) {
                let loc = format!("Mixer {} — Slot {}", ins.name, slot_idx + 1);
                if seen_warning.insert((key, loc.clone())) {
                    missing.push(MissingPluginWarning {
                        plugin_name: fx.clone(),
                        location: loc,
                    });
                }
            }
        }
    }

    let fl_version_mismatch = match (&manifest.fl_version, &peer.fl_version) {
        (proj_ver, Some(peer_ver)) if !proj_ver.is_empty() && !peer_ver.is_empty() => {
            if is_project_newer_fl_version(proj_ver, peer_ver) {
                Some(format!(
                    "Project saved in FL Studio {proj_ver}, but {} has {peer_ver}",
                    peer.display_name
                ))
            } else {
                None
            }
        }
        _ => None,
    };

    CompatibilityReport {
        peer_display_name: peer.display_name.clone(),
        missing_plugins: missing,
        fl_version_mismatch,
    }
}

fn parse_version_tuple(v: &str) -> (u32, u32, u32) {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn is_project_newer_fl_version(project_ver: &str, peer_ver: &str) -> bool {
    let (pmaj, pmin, _) = parse_version_tuple(project_ver);
    let (cmaj, cmin, _) = parse_version_tuple(peer_ver);
    (pmaj, pmin) > (cmaj, cmin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flint_core::{ChannelInfo, MixerInsertInfo};

    #[test]
    fn reads_plugin_names_from_fl_studio_plugin_database() {
        let root = std::env::temp_dir().join(format!(
            "flint_fl_db_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let vst3 = root.join("Effects").join("VST3");
        let gens = root.join("Generators").join("VST");
        std::fs::create_dir_all(&vst3).unwrap();
        std::fs::create_dir_all(&gens).unwrap();
        std::fs::write(vst3.join("RC-20 Retro Color.nfo"), b"").unwrap();
        std::fs::write(gens.join("Serum (x64).fst"), b"").unwrap();
        std::fs::write(gens.join("readme.txt"), b"").unwrap();

        let names = scan_fl_plugin_database(&root);
        assert_eq!(names, vec!["RC-20 Retro Color".to_string(), "Serum".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stock_plugins_are_not_flagged_as_missing() {
        let manifest = ProjectManifest {
            fl_version: "24.2.2.4597".into(),
            channels: vec![
                ChannelInfo {
                    index: 0,
                    name: "3x Osc".into(),
                    plugin_name: Some("3x Osc".into()),
                    wrapped_plugin_name: None,
                    sample_path: None,
                    ..Default::default()
                },
                ChannelInfo {
                    index: 1,
                    name: "Lead".into(),
                    plugin_name: Some("Fruity Wrapper".into()),
                    wrapped_plugin_name: Some("Serum 2".into()),
                    sample_path: None,
                    ..Default::default()
                },
            ],
            mixer_inserts: vec![MixerInsertInfo {
                index: 2,
                name: "Insert 2".into(),
                fx_plugins: vec![
                    "Fruity Parametric EQ 2".into(),
                    "RC-20 Retro Color".into(),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        let peer = PeerEnvironment {
            display_name: "Alex".into(),
            fl_version: Some("24.1.0".into()),
            installed_plugins: vec!["Serum2".into()], // Matches "Serum 2"!
            ..Default::default()
        };

        let report = check_compatibility(&manifest, &peer);
        assert_eq!(report.missing_plugins.len(), 1);
        assert_eq!(report.missing_plugins[0].plugin_name, "RC-20 Retro Color");
        assert!(report.missing_plugins[0].location.contains("Mixer Insert 2"));
        assert!(report.fl_version_mismatch.is_some());
    }
}
