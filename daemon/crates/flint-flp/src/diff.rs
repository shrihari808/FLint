use flint_core::{
    ArrangementInfo, ChannelInfo, MixerInsertInfo, MusicalDiff, PatternInfo, ProjectManifest,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Pair items from an old and a new list so that inserting/deleting one item does not make
/// every later item look "changed".
///
/// Matching runs in passes, each only over items not matched yet:
/// 1. `identity` key (e.g. name + plugin + sample) — the item is unchanged but may have moved.
/// 2. `name` key, only when that name is unique on both sides — the item was edited in place.
/// 3. Same index — fallback for unnamed / duplicate-named items.
///
/// Returns `(old, new)` pairs; `(Some, None)` = removed, `(None, Some)` = added.
fn pair_items<'a, T>(
    old: &'a [T],
    new: &'a [T],
    identity: impl Fn(&T) -> String,
    name: impl Fn(&T) -> String,
    index: impl Fn(&T) -> u16,
) -> Vec<(Option<&'a T>, Option<&'a T>)> {
    let mut old_used = vec![false; old.len()];
    let mut new_used = vec![false; new.len()];
    let mut pairs: Vec<(usize, usize)> = Vec::new();

    // Pass 1: identical identity key (first unused match, in order).
    for (ni, n) in new.iter().enumerate() {
        let key = identity(n);
        if let Some(oi) = (0..old.len()).find(|&oi| !old_used[oi] && identity(&old[oi]) == key) {
            old_used[oi] = true;
            new_used[ni] = true;
            pairs.push((oi, ni));
        }
    }

    // Pass 2: unique names.
    let count_names = |items: &[T], used: &[bool]| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for (i, it) in items.iter().enumerate() {
            if !used[i] {
                *m.entry(name(it)).or_default() += 1;
            }
        }
        m
    };
    let old_names = count_names(old, &old_used);
    let new_names = count_names(new, &new_used);
    for ni in 0..new.len() {
        if new_used[ni] {
            continue;
        }
        let key = name(&new[ni]);
        if key.is_empty() || new_names.get(&key) != Some(&1) || old_names.get(&key) != Some(&1) {
            continue;
        }
        if let Some(oi) = (0..old.len()).find(|&oi| !old_used[oi] && name(&old[oi]) == key) {
            old_used[oi] = true;
            new_used[ni] = true;
            pairs.push((oi, ni));
        }
    }

    // Pass 3: same index.
    for ni in 0..new.len() {
        if new_used[ni] {
            continue;
        }
        let idx = index(&new[ni]);
        if let Some(oi) = (0..old.len()).find(|&oi| !old_used[oi] && index(&old[oi]) == idx) {
            old_used[oi] = true;
            new_used[ni] = true;
            pairs.push((oi, ni));
        }
    }

    let mut out: Vec<(Option<&T>, Option<&T>)> = pairs
        .into_iter()
        .map(|(oi, ni)| (Some(&old[oi]), Some(&new[ni])))
        .collect();
    out.extend(
        (0..new.len())
            .filter(|&i| !new_used[i])
            .map(|i| (None, Some(&new[i]))),
    );
    out.extend(
        (0..old.len())
            .filter(|&i| !old_used[i])
            .map(|i| (Some(&old[i]), None)),
    );
    // Stable, readable order: by new index, then removed items by old index.
    out.sort_by_key(|(o, n)| match (o, n) {
        (_, Some(n)) => (0u8, index(n)),
        (Some(o), None) => (1u8, index(o)),
        (None, None) => (2u8, 0),
    });
    out
}

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

    // 7. Playlist arrangements (clip placement)
    diff_arrangements(&old.arrangements, &new.arrangements, &mut diff);

    // 8. Audio Samples
    diff_samples(&old.samples, &new.samples, &mut diff);

    diff
}

fn diff_channels(old: &[ChannelInfo], new: &[ChannelInfo], diff: &mut MusicalDiff) {
    let pairs = pair_items(
        old,
        new,
        |c| {
            format!(
                "{}\u{1f}{}\u{1f}{}",
                c.name,
                c.effective_plugin_name().unwrap_or(""),
                c.sample_path.as_deref().map(short_filename).unwrap_or_default()
            )
        },
        |c| c.name.clone(),
        |c| c.index,
    );

    let mut moved = 0usize;
    for (old_ch, new_ch) in pairs {
        match (old_ch, new_ch) {
            (None, Some(ch)) => {
                let label = format_channel_label(ch);
                diff.added_channels.push(label.clone());
                if let Some(plugin) = ch.effective_plugin_name() {
                    diff.added_plugins
                        .push(format!("{plugin} (Ch {}: '{}')", ch.index + 1, ch.name));
                    diff.summary_lines
                        .push(format!("Added {plugin} on Ch {} ('{}')", ch.index + 1, ch.name));
                } else {
                    diff.summary_lines.push(format!("Added channel {label}"));
                }
            }
            (Some(ch), None) => {
                let label = format_channel_label(ch);
                diff.removed_channels.push(label.clone());
                if let Some(plugin) = ch.effective_plugin_name() {
                    diff.removed_plugins
                        .push(format!("{plugin} (Ch {}: '{}')", ch.index + 1, ch.name));
                }
                diff.summary_lines.push(format!("Removed channel {label}"));
            }
            (Some(old_ch), Some(new_ch)) => {
                if old_ch.index != new_ch.index {
                    moved += 1;
                }
                let idx = new_ch.index;
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
                } else {
                    if old_ch.name != new_ch.name {
                        diff.summary_lines.push(format!(
                            "Renamed Ch {}: '{}' → '{}'",
                            idx + 1,
                            old_ch.name,
                            new_ch.name
                        ));
                    }
                    // Same plugin, different saved state => knobs/preset changed.
                    if let (Some(a), Some(b)) = (&old_ch.plugin_state_hash, &new_ch.plugin_state_hash) {
                        if a != b {
                            let plugin = new_plug.unwrap_or("plugin");
                            let entry = format!("{plugin} (Ch {}: '{}')", idx + 1, new_ch.name);
                            diff.changed_plugin_settings.push(entry);
                            diff.summary_lines.push(format!(
                                "Ch {} ('{}'): {plugin} settings changed",
                                idx + 1,
                                new_ch.name
                            ));
                        }
                    }
                }
            }
            (None, None) => {}
        }
    }

    if moved > 0 {
        diff.summary_lines.push(format!(
            "Channel rack reordered ({moved} channel{} moved)",
            if moved == 1 { "" } else { "s" }
        ));
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
            // Same FX chain: report slots whose saved state changed.
            if let (Some(o), Some(n)) = (old_ins, new_ins) {
                for (slot, fx) in new_fx.iter().enumerate() {
                    let a = o.fx_state_hashes.get(slot).filter(|h| !h.is_empty());
                    let b = n.fx_state_hashes.get(slot).filter(|h| !h.is_empty());
                    if let (Some(a), Some(b)) = (a, b) {
                        if a != b {
                            diff.changed_plugin_settings
                                .push(format!("{fx} ({track_label})"));
                            diff.summary_lines
                                .push(format!("Mixer {track_label}: {fx} settings changed"));
                        }
                    }
                }
            }
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
    let pairs = pair_items(
        old,
        new,
        |p| format!("{}\u{1f}{}", p.name, p.notes_hash),
        |p| p.name.clone(),
        |p| p.index,
    );

    for (old_pat, new_pat) in pairs {
        match (old_pat, new_pat) {
            (None, Some(pat)) => {
                let entry = format!("'{}' ({} notes)", pat.name, pat.note_count);
                diff.added_patterns.push(entry.clone());
                diff.summary_lines.push(format!("Added pattern {entry}"));
            }
            (Some(old_pat), None) => {
                diff.removed_patterns.push(old_pat.name.clone());
                diff.summary_lines
                    .push(format!("Removed pattern '{}'", old_pat.name));
            }
            (Some(old_pat), Some(pat)) => {
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
                }
                if name_changed {
                    diff.summary_lines.push(format!(
                        "Renamed pattern {}: '{}' → '{}'",
                        pat.index, old_pat.name, pat.name
                    ));
                }
            }
            (None, None) => {}
        }
    }
}

fn diff_arrangements(old: &[ArrangementInfo], new: &[ArrangementInfo], diff: &mut MusicalDiff) {
    // Manifests written before arrangements were tracked have an empty list; don't report
    // every arrangement as "added" when diffing against one of those.
    if old.is_empty() {
        return;
    }
    let pairs = pair_items(
        old,
        new,
        |a| format!("{}\u{1f}{}", a.name, a.playlist_hash),
        |a| a.name.clone(),
        |a| a.index,
    );
    for (o, n) in pairs {
        match (o, n) {
            (None, Some(a)) => diff
                .summary_lines
                .push(format!("Added arrangement '{}'", a.name)),
            (Some(a), None) => diff
                .summary_lines
                .push(format!("Removed arrangement '{}'", a.name)),
            (Some(o), Some(n)) => {
                if o.playlist_hash != n.playlist_hash {
                    let detail = match n.playlist_bytes.cmp(&o.playlist_bytes) {
                        std::cmp::Ordering::Greater => "clips added",
                        std::cmp::Ordering::Less => "clips removed",
                        std::cmp::Ordering::Equal => "clips moved or edited",
                    };
                    diff.edited_arrangements.push(n.name.clone());
                    diff.summary_lines
                        .push(format!("Arrangement '{}': {detail}", n.name));
                }
                if o.name != n.name {
                    diff.summary_lines.push(format!(
                        "Renamed arrangement '{}' → '{}'",
                        o.name, n.name
                    ));
                }
            }
            (None, None) => {}
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
                ..Default::default()
            }],
            mixer_inserts: vec![MixerInsertInfo {
                index: 2,
                name: "Insert 2".into(),
                fx_plugins: vec!["Fruity Filter".into()],
                ..Default::default()
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
                    ..Default::default()
                },
            ],
            mixer_inserts: vec![MixerInsertInfo {
                index: 2,
                name: "Insert 2".into(),
                fx_plugins: vec!["Fruity Filter".into(), "RC-20 Retro Color".into()],
                ..Default::default()
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

    fn ch(index: u16, name: &str, plugin: Option<&str>, state: Option<&str>) -> ChannelInfo {
        ChannelInfo {
            index,
            name: name.into(),
            plugin_name: plugin.map(Into::into),
            plugin_state_hash: state.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn inserting_a_channel_does_not_make_later_channels_look_changed() {
        let old = ProjectManifest {
            channels: vec![
                ch(0, "Kick", None, None),
                ch(1, "Snare", None, None),
                ch(2, "Bass", Some("3x Osc"), Some("s1")),
            ],
            ..Default::default()
        };
        let new = ProjectManifest {
            channels: vec![
                ch(0, "Clap", None, None),
                ch(1, "Kick", None, None),
                ch(2, "Snare", None, None),
                ch(3, "Bass", Some("3x Osc"), Some("s1")),
            ],
            ..Default::default()
        };
        let d = diff_manifests(&old, &new);
        assert_eq!(d.added_channels, vec!["Ch 1 ('Clap')".to_string()], "{d:?}");
        assert!(d.removed_channels.is_empty(), "{d:?}");
        assert!(!d.summary_lines.iter().any(|l| l.contains("Renamed")), "{d:?}");
        assert!(d.summary_lines.iter().any(|l| l.contains("reordered")), "{d:?}");
    }

    #[test]
    fn reports_plugin_settings_changes() {
        let old = ProjectManifest {
            channels: vec![ch(0, "Lead", Some("Sytrus"), Some("aaaa"))],
            mixer_inserts: vec![MixerInsertInfo {
                index: 1,
                name: "Vox".into(),
                fx_plugins: vec!["Fruity Limiter".into()],
                fx_state_hashes: vec!["1111".into()],
            }],
            ..Default::default()
        };
        let new = ProjectManifest {
            channels: vec![ch(0, "Lead", Some("Sytrus"), Some("bbbb"))],
            mixer_inserts: vec![MixerInsertInfo {
                index: 1,
                name: "Vox".into(),
                fx_plugins: vec!["Fruity Limiter".into()],
                fx_state_hashes: vec!["2222".into()],
            }],
            ..Default::default()
        };
        let d = diff_manifests(&old, &new);
        assert!(d.summary_lines.iter().any(|l| l == "Ch 1 ('Lead'): Sytrus settings changed"), "{d:?}");
        assert!(d.summary_lines.iter().any(|l| l == "Mixer Vox: Fruity Limiter settings changed"), "{d:?}");
        assert_eq!(d.changed_plugin_settings.len(), 2);
    }

    #[test]
    fn reports_arrangement_edits_but_not_against_legacy_manifests() {
        let arr = |hash: &str, bytes: usize| ArrangementInfo {
            index: 0,
            name: "Main".into(),
            playlist_bytes: bytes,
            playlist_hash: hash.into(),
        };
        let old = ProjectManifest {
            arrangements: vec![arr("aaaa", 64)],
            ..Default::default()
        };
        let new = ProjectManifest {
            arrangements: vec![arr("bbbb", 96)],
            ..Default::default()
        };
        let d = diff_manifests(&old, &new);
        assert!(d.summary_lines.iter().any(|l| l == "Arrangement 'Main': clips added"), "{d:?}");

        let legacy = ProjectManifest::default();
        assert!(diff_manifests(&legacy, &new).is_empty());
    }

    #[test]
    fn patterns_are_matched_by_name_when_renumbered() {
        let p = |index: u16, name: &str, hash: &str| PatternInfo {
            index,
            name: name.into(),
            note_count: 4,
            notes_hash: hash.into(),
        };
        let old = ProjectManifest {
            patterns: vec![p(1, "Intro", "a"), p(2, "Drop", "b")],
            ..Default::default()
        };
        let new = ProjectManifest {
            patterns: vec![p(1, "Drop", "b"), p(2, "Intro", "a")],
            ..Default::default()
        };
        assert!(diff_manifests(&old, &new).is_empty());
    }
}
