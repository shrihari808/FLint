use flint_core::{ChannelInfo, MixerInsertInfo, MusicalDiff, PatternInfo, ProjectManifest};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Compute a semantic musical diff between two `ProjectManifest` versions.
pub fn diff_manifests(old: &ProjectManifest, new: &ProjectManifest) -> MusicalDiff {
    let mut diff = MusicalDiff::default();

    // 1. Tempo
    if (old.bpm - new.bpm).abs() >= 0.001 {
        diff.bpm_changed = Some((old.bpm, new.bpm));
        diff.summary_lines.push(format!(
            "BPM: {} → {}",
            format_bpm(old.bpm),
            format_bpm(new.bpm)
        ));
    }

    // 2. Time signature
    if (old.time_sig_num, old.time_sig_den) != (new.time_sig_num, new.time_sig_den) {
        diff.time_sig_changed = Some((
            (old.time_sig_num, old.time_sig_den),
            (new.time_sig_num, new.time_sig_den),
        ));
        diff.summary_lines.push(format!(
            "Time signature: {}/{} → {}/{}",
            old.time_sig_num, old.time_sig_den, new.time_sig_num, new.time_sig_den
        ));
    }

    // 3. FL Studio version
    if !old.fl_version.is_empty()
        && !new.fl_version.is_empty()
        && old.fl_version != new.fl_version
    {
        diff.fl_version_changed = Some((old.fl_version.clone(), new.fl_version.clone()));
        diff.summary_lines.push(format!(
            "FL Studio version: {} → {}",
            old.fl_version, new.fl_version
        ));
    }

    // 4. Channels & Generator Plugins
    diff_channels(&old.channels, &new.channels, &mut diff);

    // 5. Mixer Inserts & FX Chains
    diff_mixer(&old.mixer_inserts, &new.mixer_inserts, &mut diff);

    // 6. Patterns & Notes
    diff_patterns(&old.patterns, &new.patterns, &mut diff);

    // 7. Audio Samples
    diff_samples(&old.samples, &new.samples, &mut diff);

    diff
}

fn diff_channels(old: &[ChannelInfo], new: &[ChannelInfo], diff: &mut MusicalDiff) {
    let old_map: BTreeMap<u16, &ChannelInfo> = old.iter().map(|c| (c.index, c)).collect();
    let new_map: BTreeMap<u16, &ChannelInfo> = new.iter().map(|c| (c.index, c)).collect();

    for (&idx, ch) in &new_map {
        if !old_map.contains_key(&idx) {
            let label = format_channel_label(ch);
            diff.added_channels.push(label.clone());
            if let Some(plugin) = ch.effective_plugin_name() {
                let p_entry = format!("{plugin} (Ch {}: '{}')", idx + 1, ch.name);
                diff.added_plugins.push(p_entry);
                diff.summary_lines
                    .push(format!("Added {plugin} on Ch {} ('{}')", idx + 1, ch.name));
            } else {
                diff.summary_lines
                    .push(format!("Added channel {label}"));
            }
        }
    }

    for (&idx, ch) in &old_map {
        if !new_map.contains_key(&idx) {
            let label = format_channel_label(ch);
            diff.removed_channels.push(label.clone());
            if let Some(plugin) = ch.effective_plugin_name() {
                diff.removed_plugins
                    .push(format!("{plugin} (Ch {}: '{}')", idx + 1, ch.name));
            }
            diff.summary_lines
                .push(format!("Removed channel {label}"));
        }
    }

    for (&idx, new_ch) in &new_map {
        if let Some(old_ch) = old_map.get(&idx) {
            let old_plug = old_ch.effective_plugin_name();
            let new_plug = new_ch.effective_plugin_name();
            if old_plug != new_plug {
                match (old_plug, new_plug) {
                    (Some(a), Some(b)) => {
                        diff.removed_plugins.push(format!("{a} (Ch {})", idx + 1));
                        diff.added_plugins.push(format!("{b} (Ch {})", idx + 1));
                        diff.summary_lines.push(format!(
                            "Ch {} ('{}'): swapped plugin {} → {}",
                            idx + 1,
                            new_ch.name,
                            a,
                            b
                        ));
                    }
                    (None, Some(b)) => {
                        diff.added_plugins.push(format!("{b} (Ch {})", idx + 1));
                        diff.summary_lines.push(format!(
                            "Ch {} ('{}'): loaded plugin {}",
                            idx + 1,
                            new_ch.name,
                            b
                        ));
                    }
                    (Some(a), None) => {
                        diff.removed_plugins.push(format!("{a} (Ch {})", idx + 1));
                        diff.summary_lines.push(format!(
                            "Ch {} ('{}'): removed plugin {}",
                            idx + 1,
                            new_ch.name,
                            a
                        ));
                    }
                    (None, None) => {}
                }
            } else if old_ch.name != new_ch.name {
                diff.summary_lines.push(format!(
                    "Renamed Ch {}: '{}' → '{}'",
                    idx + 1,
                    old_ch.name,
                    new_ch.name
                ));
            }
        }
    }
}

fn diff_mixer(old: &[MixerInsertInfo], new: &[MixerInsertInfo], diff: &mut MusicalDiff) {
    let old_map: BTreeMap<u16, &MixerInsertInfo> = old.iter().map(|m| (m.index, m)).collect();
    let new_map: BTreeMap<u16, &MixerInsertInfo> = new.iter().map(|m| (m.index, m)).collect();

    let all_indices: BTreeSet<u16> = old_map
        .keys()
        .chain(new_map.keys())
        .copied()
        .collect();

    for idx in all_indices {
        let old_ins = old_map.get(&idx).copied();
        let new_ins = new_map.get(&idx).copied();

        let track_label = new_ins
            .or(old_ins)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| {
                if idx == 0 {
                    "Master".to_string()
                } else {
                    format!("Insert {idx}")
                }
            });

        let old_fx = old_ins.map(|m| m.fx_plugins.as_slice()).unwrap_or(&[]);
        let new_fx = new_ins.map(|m| m.fx_plugins.as_slice()).unwrap_or(&[]);

        if old_fx == new_fx {
            continue;
        }

        let old_counts = count_strings(old_fx);
        let new_counts = count_strings(new_fx);

        let mut added = Vec::new();
        let mut removed = Vec::new();

        for (fx, &new_n) in &new_counts {
            let old_n = old_counts.get(fx).copied().unwrap_or(0);
            for _ in old_n..new_n {
                added.push(fx.clone());
                diff.added_plugins.push(format!("{fx} ({track_label})"));
            }
        }

        for (fx, &old_n) in &old_counts {
            let new_n = new_counts.get(fx).copied().unwrap_or(0);
            for _ in new_n..old_n {
                removed.push(fx.clone());
                diff.removed_plugins.push(format!("{fx} ({track_label})"));
            }
        }

        if !added.is_empty() || !removed.is_empty() {
            let mut parts = Vec::new();
            if !added.is_empty() {
                parts.push(format!("+{}", added.join(", +")));
            }
            if !removed.is_empty() {
                parts.push(format!("-{}", removed.join(", -")));
            }
            diff.summary_lines
                .push(format!("Mixer {track_label}: {}", parts.join(", ")));
        } else {
            diff.summary_lines
                .push(format!("Mixer {track_label}: reordered FX chain"));
        }
    }
}

fn diff_patterns(old: &[PatternInfo], new: &[PatternInfo], diff: &mut MusicalDiff) {
    let old_map: BTreeMap<u16, &PatternInfo> = old.iter().map(|p| (p.index, p)).collect();
    let new_map: BTreeMap<u16, &PatternInfo> = new.iter().map(|p| (p.index, p)).collect();

    for (&idx, pat) in &new_map {
        match old_map.get(&idx) {
            None => {
                let entry = format!("'{}' ({} notes)", pat.name, pat.note_count);
                diff.added_patterns.push(entry.clone());
                diff.summary_lines.push(format!("Added pattern {entry}"));
            }
            Some(old_pat) => {
                let notes_changed = old_pat.notes_hash != pat.notes_hash
                    || old_pat.note_count != pat.note_count;
                let name_changed = old_pat.name != pat.name;

                if notes_changed {
                    let delta = pat.note_count as isize - old_pat.note_count as isize;
                    let detail = if delta > 0 {
                        format!("+{delta} notes")
                    } else if delta < 0 {
                        format!("{delta} notes")
                    } else {
                        "notes edited".to_string()
                    };
                    let entry = format!("'{}' ({detail})", pat.name);
                    diff.modified_patterns.push(entry.clone());
                    diff.summary_lines.push(format!("Edited pattern {entry}"));
                } else if name_changed {
                    diff.summary_lines.push(format!(
                        "Renamed pattern {}: '{}' → '{}'",
                        idx, old_pat.name, pat.name
                    ));
                }
            }
        }
    }

    for (&idx, old_pat) in &old_map {
        if !new_map.contains_key(&idx) {
            diff.removed_patterns.push(old_pat.name.clone());
            diff.summary_lines
                .push(format!("Removed pattern '{}'", old_pat.name));
        }
    }
}

fn diff_samples(old: &[String], new: &[String], diff: &mut MusicalDiff) {
    // Compare by normalized filename so Windows <-> macOS path prefixes never cause false diffs.
    let old_set: BTreeSet<String> = old.iter().map(|p| short_filename(p)).collect();
    let new_set: BTreeSet<String> = new.iter().map(|p| short_filename(p)).collect();

    let added: Vec<String> = new_set.difference(&old_set).cloned().collect();
    let removed: Vec<String> = old_set.difference(&new_set).cloned().collect();

    if !added.is_empty() {
        diff.added_samples = added.clone();
        diff.summary_lines
            .push(format!("Added samples: {}", added.join(", ")));
    }
    if !removed.is_empty() {
        diff.removed_samples = removed.clone();
        diff.summary_lines
            .push(format!("Removed samples: {}", removed.join(", ")));
    }
}

fn count_strings(items: &[String]) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    for s in items {
        *map.entry(s.clone()).or_insert(0) += 1;
    }
    map
}

fn format_bpm(bpm: f64) -> String {
    if (bpm - bpm.round()).abs() < 0.001 {
        format!("{:.0}", bpm)
    } else {
        format!("{bpm:.2}")
    }
}

fn format_channel_label(ch: &ChannelInfo) -> String {
    format!("Ch {} ('{}')", ch.index + 1, ch.name)
}

fn short_filename(raw: &str) -> String {
    // Handle both Windows backslash and POSIX slash paths cleanly.
    let tail = raw.rsplit(['\\', '/']).next().unwrap_or(raw);
    Path::new(tail)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(raw)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_semantic_musical_diff() {
        let old = ProjectManifest {
            fl_version: "24.2.2".into(),
            bpm: 124.0,
            channels: vec![ChannelInfo {
                index: 0,
                name: "Kick".into(),
                plugin_name: None,
                wrapped_plugin_name: None,
                sample_path: Some("Samples\\Kick.wav".into()),
            }],
            mixer_inserts: vec![MixerInsertInfo {
                index: 2,
                name: "Insert 2".into(),
                fx_plugins: vec!["Fruity Filter".into()],
            }],
            patterns: vec![PatternInfo {
                index: 1,
                name: "bass".into(),
                note_count: 8,
                notes_hash: "aaaa".into(),
            }],
            samples: vec!["Samples\\Kick.wav".into()],
            ..Default::default()
        };

        let new = ProjectManifest {
            fl_version: "24.2.2".into(),
            bpm: 128.0,
            channels: vec![
                old.channels[0].clone(),
                ChannelInfo {
                    index: 1,
                    name: "Lead".into(),
                    plugin_name: Some("Fruity Wrapper".into()),
                    wrapped_plugin_name: Some("Serum 2".into()),
                    sample_path: None,
                },
            ],
            mixer_inserts: vec![MixerInsertInfo {
                index: 2,
                name: "Insert 2".into(),
                fx_plugins: vec!["Fruity Filter".into(), "RC-20 Retro Color".into()],
            }],
            patterns: vec![PatternInfo {
                index: 1,
                name: "bass".into(),
                note_count: 14,
                notes_hash: "bbbb".into(),
            }],
            samples: vec!["Samples\\Kick.wav".into(), "Samples\\Vox.wav".into()],
            ..Default::default()
        };

        let diff = diff_manifests(&old, &new);
        assert_eq!(diff.bpm_changed, Some((124.0, 128.0)));
        assert!(diff.summary_lines.iter().any(|l| l.contains("BPM: 124 → 128")));
        assert!(diff.summary_lines.iter().any(|l| l.contains("Added Serum 2")));
        assert!(diff
            .summary_lines
            .iter()
            .any(|l| l.contains("Mixer Insert 2: +RC-20 Retro Color")));
        assert!(diff
            .summary_lines
            .iter()
            .any(|l| l.contains("Edited pattern 'bass' (+6 notes)")));
        assert!(diff
            .summary_lines
            .iter()
            .any(|l| l.contains("Added samples: Vox.wav")));
    }

    #[test]
    fn sample_diff_ignores_macos_vs_windows_path_prefixes() {
        let mac = ProjectManifest {
            samples: vec!["/var/folders/tmp/Image-Line/Kick.wav".into()],
            ..Default::default()
        };
        let win = ProjectManifest {
            samples: vec![r"C:\Users\kshri\Music\Project\Samples\Kick.wav".into()],
            ..Default::default()
        };
        let diff = diff_manifests(&mac, &win);
        assert!(diff.is_empty(), "expected no sample diff across OS paths: {diff:?}");
    }
}
