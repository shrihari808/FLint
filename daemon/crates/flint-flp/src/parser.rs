use crate::FlpError;
use flint_core::{ChannelInfo, MixerInsertInfo, PatternInfo, ProjectManifest};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

// FLP Event IDs (verified against FL Studio 24.2.2 and PyFLP specification)
const BYTE_TIME_SIG_NUM: u8 = 17;
const BYTE_TIME_SIG_DEN: u8 = 18;

const WORD_NEW_CHAN: u8 = 64;
const WORD_NEW_PAT: u8 = 65;
const WORD_COARSE_TEMPO: u8 = 66;
const WORD_SLOT_INDEX: u8 = 98;

const DWORD_FINE_TEMPO: u8 = 156;

const TEXT_PAT_NAME: u8 = 193;
const TEXT_TITLE: u8 = 194;
const TEXT_SAMPLE_PATH: u8 = 196;
const TEXT_VERSION: u8 = 199;
const TEXT_PLUGIN_NAME: u8 = 201;
const TEXT_DATA_PATH: u8 = 202;
const TEXT_DISPLAY_NAME: u8 = 203;
const TEXT_INSERT_NAME: u8 = 204;

const DATA_PLUGIN_STATE: u8 = 213;
const DATA_PATTERN_NOTES: u8 = 224;
const DATA_INSERT_PARAMS: u8 = 236;

/// Size of a single note record in `DATA_PATTERN_NOTES` (FL Studio 20+).
const NOTE_RECORD_BYTES: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseContext {
    Project,
    Channel(u16),
    MixerInsert(u16),
}

#[derive(Debug, Default)]
struct ChannelBuilder {
    index: u16,
    name: String,
    plugin_name: Option<String>,
    wrapped_plugin_name: Option<String>,
    sample_path: Option<String>,
}

#[derive(Debug, Default)]
struct PatternBuilder {
    index: u16,
    name: String,
    note_count: usize,
    notes_hash: String,
}

#[derive(Debug, Default)]
struct MixerInsertBuilder {
    index: u16,
    name: String,
    fx_plugins: Vec<String>,
}

#[derive(Debug, Default)]
struct PendingSlot {
    plugin_name: Option<String>,
    display_name: Option<String>,
    wrapped_plugin_name: Option<String>,
}

impl PendingSlot {
    fn is_present(&self) -> bool {
        self.plugin_name.as_deref().is_some_and(|s| !s.is_empty())
            || self
                .wrapped_plugin_name
                .as_deref()
                .is_some_and(|s| !s.is_empty())
    }

    fn resolved_name(&self) -> Option<String> {
        if let Some(wrapped) = self.wrapped_plugin_name.as_deref().filter(|s| !s.is_empty()) {
            return Some(wrapped.to_string());
        }
        if let Some(plugin) = self.plugin_name.as_deref().filter(|s| !s.is_empty()) {
            if plugin.eq_ignore_ascii_case("Fruity Wrapper") {
                if let Some(display) = self.display_name.as_deref().filter(|s| !s.is_empty()) {
                    return Some(display.to_string());
                }
            }
            return Some(plugin.to_string());
        }
        None
    }
}

/// Parse an FL Studio `.flp` file directly, or extract and parse the `.flp` inside a Zipped Loop `.zip`.
pub fn parse_flp_path(path: impl AsRef<Path>) -> Result<ProjectManifest, FlpError> {
    let path = path.as_ref();
    let is_zip = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));

    if is_zip {
        let file = File::open(path)?;
        let mut archive = zip::ZipArchive::new(BufReader::new(file))?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            if entry
                .name()
                .rsplit('/')
                .next()
                .is_some_and(|n| n.to_ascii_lowercase().ends_with(".flp"))
            {
                let mut bytes = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut bytes)?;
                return parse_flp_bytes(&bytes);
            }
        }
        return Err(FlpError::NoFlpInZip);
    }

    let bytes = std::fs::read(path)?;
    parse_flp_bytes(&bytes)
}

/// Parse an `.flp` binary buffer into a normalized `ProjectManifest`.
pub fn parse_flp_bytes(bytes: &[u8]) -> Result<ProjectManifest, FlpError> {
    if bytes.len() < 22 {
        return Err(FlpError::UnexpectedEof(bytes.len()));
    }
    if &bytes[0..4] != b"FLhd" {
        return Err(FlpError::InvalidHeader);
    }
    let header_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let data_chunk_offset = 8 + header_len;
    if bytes.len() < data_chunk_offset + 8 {
        return Err(FlpError::UnexpectedEof(bytes.len()));
    }

    let _format = i16::from_le_bytes(bytes[8..10].try_into().unwrap());
    let _header_channels = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
    let ppq = u16::from_le_bytes(bytes[12..14].try_into().unwrap());

    if &bytes[data_chunk_offset..data_chunk_offset + 4] != b"FLdt" {
        return Err(FlpError::InvalidDataChunk);
    }
    let data_len = u32::from_le_bytes(
        bytes[data_chunk_offset + 4..data_chunk_offset + 8]
            .try_into()
            .unwrap(),
    ) as usize;

    let mut pos = data_chunk_offset + 8;
    let end = (pos + data_len).min(bytes.len());

    let mut manifest = ProjectManifest {
        ppq,
        ..ProjectManifest::default()
    };

    let mut is_unicode = true; // Updated once TEXT_VERSION (199) is parsed
    let mut seen_fine_tempo = false;
    let mut ctx = ParseContext::Project;
    let mut current_pattern: Option<u16> = None;
    let mut next_mixer_insert: u16 = 0;

    let mut channels: BTreeMap<u16, ChannelBuilder> = BTreeMap::new();
    let mut patterns: BTreeMap<u16, PatternBuilder> = BTreeMap::new();
    let mut mixer_inserts: BTreeMap<u16, MixerInsertBuilder> = BTreeMap::new();
    let mut sample_set: BTreeSet<String> = BTreeSet::new();
    let mut pending_slot = PendingSlot::default();

    while pos < end {
        let id = bytes[pos];
        pos += 1;

        match id {
            0..=63 => {
                if pos + 1 > end {
                    return Err(FlpError::UnexpectedEof(pos));
                }
                let val = bytes[pos];
                pos += 1;
                match id {
                    BYTE_TIME_SIG_NUM if val > 0 => manifest.time_sig_num = val,
                    BYTE_TIME_SIG_DEN if val > 0 => manifest.time_sig_den = val,
                    _ => {}
                }
            }
            64..=127 => {
                if pos + 2 > end {
                    return Err(FlpError::UnexpectedEof(pos));
                }
                let val = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap());
                pos += 2;
                match id {
                    WORD_NEW_CHAN => {
                        ctx = ParseContext::Channel(val);
                        channels.entry(val).or_insert_with(|| ChannelBuilder {
                            index: val,
                            ..Default::default()
                        });
                    }
                    WORD_NEW_PAT => {
                        current_pattern = Some(val);
                        patterns.entry(val).or_insert_with(|| PatternBuilder {
                            index: val,
                            ..Default::default()
                        });
                    }
                    WORD_COARSE_TEMPO if !seen_fine_tempo => {
                        manifest.bpm = f64::from(val);
                    }
                    WORD_SLOT_INDEX => {
                        if let ParseContext::MixerInsert(ins_idx) = ctx {
                            if pending_slot.is_present() {
                                if let Some(name) = pending_slot.resolved_name() {
                                    mixer_inserts
                                        .entry(ins_idx)
                                        .or_insert_with(|| MixerInsertBuilder {
                                            index: ins_idx,
                                            ..Default::default()
                                        })
                                        .fx_plugins
                                        .push(name);
                                }
                            }
                            pending_slot = PendingSlot::default();
                        }
                    }
                    _ => {}
                }
            }
            128..=191 => {
                if pos + 4 > end {
                    return Err(FlpError::UnexpectedEof(pos));
                }
                let val = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
                pos += 4;
                if id == DWORD_FINE_TEMPO && val > 0 {
                    manifest.bpm = f64::from(val) / 1000.0;
                    seen_fine_tempo = true;
                }
            }
            192..=255 => {
                let len = read_varint(bytes, &mut pos, end)?;
                if pos + len > end {
                    return Err(FlpError::UnexpectedEof(pos));
                }
                let payload = &bytes[pos..pos + len];
                pos += len;

                match id {
                    TEXT_VERSION => {
                        let ver = decode_ascii(payload);
                        is_unicode = fl_version_uses_utf16(&ver);
                        manifest.fl_version = ver;
                    }
                    TEXT_TITLE => {
                        manifest.title = decode_flp_string(payload, is_unicode);
                    }
                    TEXT_DATA_PATH => {
                        let s = decode_flp_string(payload, is_unicode);
                        if !s.is_empty() {
                            manifest.data_path = Some(s);
                        }
                    }
                    TEXT_PAT_NAME => {
                        if let Some(pat_idx) = current_pattern {
                            let s = decode_flp_string(payload, is_unicode);
                            patterns
                                .entry(pat_idx)
                                .or_insert_with(|| PatternBuilder {
                                    index: pat_idx,
                                    ..Default::default()
                                })
                                .name = s;
                        }
                    }
                    DATA_PATTERN_NOTES => {
                        if let Some(pat_idx) = current_pattern {
                            let entry = patterns.entry(pat_idx).or_insert_with(|| PatternBuilder {
                                index: pat_idx,
                                ..Default::default()
                            });
                            entry.note_count = payload.len() / NOTE_RECORD_BYTES;
                            entry.notes_hash = blake3::hash(payload).to_hex()[..16].to_string();
                        }
                    }
                    TEXT_SAMPLE_PATH => {
                        let s = decode_flp_string(payload, is_unicode);
                        if !s.is_empty() {
                            sample_set.insert(s.clone());
                            if let ParseContext::Channel(ch_idx) = ctx {
                                if let Some(ch) = channels.get_mut(&ch_idx) {
                                    ch.sample_path = Some(s);
                                }
                            }
                        }
                    }
                    TEXT_PLUGIN_NAME => {
                        let s = decode_flp_string(payload, is_unicode);
                        match ctx {
                            ParseContext::Channel(ch_idx) => {
                                if let Some(ch) = channels.get_mut(&ch_idx) {
                                    if !s.is_empty() {
                                        ch.plugin_name = Some(s);
                                    }
                                }
                            }
                            ParseContext::MixerInsert(_) => {
                                pending_slot = PendingSlot {
                                    plugin_name: (!s.is_empty()).then_some(s),
                                    ..Default::default()
                                };
                            }
                            ParseContext::Project => {}
                        }
                    }
                    TEXT_DISPLAY_NAME => {
                        let s = decode_flp_string(payload, is_unicode);
                        match ctx {
                            ParseContext::Channel(ch_idx) => {
                                if let Some(ch) = channels.get_mut(&ch_idx) {
                                    if !s.is_empty() {
                                        ch.name = s;
                                    }
                                }
                            }
                            ParseContext::MixerInsert(_) => {
                                if !s.is_empty() {
                                    pending_slot.display_name = Some(s);
                                }
                            }
                            ParseContext::Project => {}
                        }
                    }
                    DATA_PLUGIN_STATE => match ctx {
                        ParseContext::Channel(ch_idx) => {
                            if let Some(ch) = channels.get_mut(&ch_idx) {
                                let is_wrapper = ch
                                    .plugin_name
                                    .as_deref()
                                    .is_some_and(|p| p.eq_ignore_ascii_case("Fruity Wrapper"));
                                if is_wrapper {
                                    if let Some(vst_name) = parse_fruity_wrapper_name(payload) {
                                        ch.wrapped_plugin_name = Some(vst_name);
                                    }
                                }
                            }
                        }
                        ParseContext::MixerInsert(_) => {
                            let is_wrapper = pending_slot
                                .plugin_name
                                .as_deref()
                                .is_some_and(|p| p.eq_ignore_ascii_case("Fruity Wrapper"));
                            if is_wrapper {
                                if let Some(vst_name) = parse_fruity_wrapper_name(payload) {
                                    pending_slot.wrapped_plugin_name = Some(vst_name);
                                }
                            }
                        }
                        ParseContext::Project => {}
                    },
                    DATA_INSERT_PARAMS => {
                        let ins_idx = next_mixer_insert;
                        next_mixer_insert = next_mixer_insert.saturating_add(1);
                        ctx = ParseContext::MixerInsert(ins_idx);
                        pending_slot = PendingSlot::default();
                        mixer_inserts.entry(ins_idx).or_insert_with(|| MixerInsertBuilder {
                            index: ins_idx,
                            ..Default::default()
                        });
                    }
                    TEXT_INSERT_NAME => {
                        if let ParseContext::MixerInsert(ins_idx) = ctx {
                            let s = decode_flp_string(payload, is_unicode);
                            if !s.is_empty() {
                                mixer_inserts
                                    .entry(ins_idx)
                                    .or_insert_with(|| MixerInsertBuilder {
                                        index: ins_idx,
                                        ..Default::default()
                                    })
                                    .name = s;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    manifest.channels = channels
        .into_values()
        .map(|mut c| {
            // If Fruity Wrapper didn't yield an explicit subchunk name, fall back to display name.
            if c.wrapped_plugin_name.is_none()
                && c.plugin_name
                    .as_deref()
                    .is_some_and(|p| p.eq_ignore_ascii_case("Fruity Wrapper"))
                && !c.name.is_empty()
            {
                c.wrapped_plugin_name = Some(c.name.clone());
            }
            if c.name.is_empty() {
                c.name = c
                    .wrapped_plugin_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or_else(|| c.plugin_name.as_deref().filter(|s| !s.is_empty()))
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Channel {}", c.index + 1));
            }
            ChannelInfo {
                index: c.index,
                name: c.name,
                plugin_name: c.plugin_name,
                wrapped_plugin_name: c.wrapped_plugin_name,
                sample_path: c.sample_path,
            }
        })
        .collect();

    manifest.mixer_inserts = mixer_inserts
        .into_values()
        .filter(|m| !m.fx_plugins.is_empty() || !m.name.is_empty())
        .map(|m| {
            let name = if m.name.is_empty() {
                if m.index == 0 {
                    "Master".to_string()
                } else {
                    format!("Insert {}", m.index)
                }
            } else {
                m.name
            };
            MixerInsertInfo {
                index: m.index,
                name,
                fx_plugins: m.fx_plugins,
            }
        })
        .collect();

    manifest.patterns = patterns
        .into_values()
        .map(|p| PatternInfo {
            index: p.index,
            name: if p.name.is_empty() {
                format!("Pattern {}", p.index)
            } else {
                p.name
            },
            note_count: p.note_count,
            notes_hash: p.notes_hash,
        })
        .collect();

    manifest.samples = sample_set.into_iter().collect();

    Ok(manifest)
}

fn read_varint(bytes: &[u8], pos: &mut usize, end: usize) -> Result<usize, FlpError> {
    let start = *pos;
    let mut len: usize = 0;
    let mut shift: u32 = 0;
    loop {
        if *pos >= end {
            return Err(FlpError::UnexpectedEof(*pos));
        }
        let b = bytes[*pos];
        *pos += 1;
        let part = usize::from(b & 0x7F)
            .checked_shl(shift)
            .ok_or(FlpError::InvalidVarint(start))?;
        len |= part;
        if (b & 0x80) == 0 {
            return Ok(len);
        }
        shift += 7;
        if shift > 28 {
            return Err(FlpError::InvalidVarint(start));
        }
    }
}

fn decode_ascii(bytes: &[u8]) -> String {
    let trimmed = match bytes.iter().position(|&b| b == 0) {
        Some(idx) => &bytes[..idx],
        None => bytes,
    };
    String::from_utf8_lossy(trimmed).trim().to_string()
}

fn decode_flp_string(bytes: &[u8], is_unicode: bool) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if is_unicode && bytes.len() % 2 == 0 {
        let mut units = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks_exact(2) {
            let u = u16::from_le_bytes([pair[0], pair[1]]);
            if u == 0 {
                break;
            }
            units.push(u);
        }
        String::from_utf16_lossy(&units).trim().to_string()
    } else {
        decode_ascii(bytes)
    }
}

fn fl_version_uses_utf16(version: &str) -> bool {
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(20);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    major > 11 || (major == 11 && minor >= 5)
}

/// Best-effort extractor for third-party VST2/VST3 plugin name inside Fruity Wrapper `DATA_PLUGIN_STATE`.
///
/// Fruity Wrapper chunks begin with a 4-byte plugin type/version header followed by TLV sub-events
/// (`id: u32`, `length: u64`, `payload: [u8; length]`). Known name/path sub-event IDs:
/// - `30`: VST2/VST3 plugin display name (UTF-8/ASCII)
/// - `32`: VST plugin filename or path
/// - `54`: VST3/wrapper plugin name
/// - `56`: VST3/wrapper plugin path
fn parse_fruity_wrapper_name(data: &[u8]) -> Option<String> {
    if data.len() < 16 {
        return None;
    }

    // Standard wrapper state starts with 4-byte format tag (typically 8 for VST2, 10 for VST3, or version int).
    let mut pos = 4;
    let mut name_candidate: Option<String> = None;
    let mut path_candidate: Option<String> = None;

    while pos + 12 <= data.len() {
        let sub_id = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?);
        let sub_len = u64::from_le_bytes(data[pos + 4..pos + 12].try_into().ok()?) as usize;
        pos += 12;

        if sub_len > data.len().saturating_sub(pos) {
            break;
        }
        let payload = &data[pos..pos + sub_len];
        pos += sub_len;

        match sub_id {
            30 | 54 => {
                let s = decode_wrapper_text(payload);
                if !s.is_empty() {
                    name_candidate = Some(s);
                }
            }
            32 | 56 => {
                let s = decode_wrapper_text(payload);
                if !s.is_empty() {
                    path_candidate = Some(s);
                }
            }
            _ => {}
        }
    }

    name_candidate.or_else(|| {
        path_candidate.and_then(|p| {
            Path::new(&p)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
    })
}

fn decode_wrapper_text(bytes: &[u8]) -> String {
    // Handle both UTF-8/ASCII and occasional UTF-16LE payloads cleanly.
    if bytes.len() >= 4 && bytes.len() % 2 == 0 && bytes[1] == 0 && bytes[0].is_ascii_graphic() {
        return decode_flp_string(bytes, true);
    }
    decode_ascii(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_varint(out: &mut Vec<u8>, mut len: usize) {
        loop {
            let mut byte = (len & 0x7F) as u8;
            len >>= 7;
            if len > 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if len == 0 {
                break;
            }
        }
    }

    fn push_utf16_event(out: &mut Vec<u8>, id: u8, s: &str) {
        let mut raw = Vec::new();
        for u in s.encode_utf16() {
            raw.extend_from_slice(&u.to_le_bytes());
        }
        raw.extend_from_slice(&0u16.to_le_bytes());
        out.push(id);
        push_varint(out, raw.len());
        out.extend_from_slice(&raw);
    }

    fn build_synthetic_flp(bpm_milli: u32, extra_fx: Option<&str>) -> Vec<u8> {
        let mut dt = Vec::new();

        // FL version (ASCII)
        dt.push(TEXT_VERSION);
        let ver = b"24.2.2.4597\0";
        push_varint(&mut dt, ver.len());
        dt.extend_from_slice(ver);

        // Fine tempo
        dt.push(DWORD_FINE_TEMPO);
        dt.extend_from_slice(&bpm_milli.to_le_bytes());

        // Time sig 4/4
        dt.extend_from_slice(&[BYTE_TIME_SIG_NUM, 4, BYTE_TIME_SIG_DEN, 4]);

        // Pattern 1 with 2 notes (48 bytes)
        dt.push(WORD_NEW_PAT);
        dt.extend_from_slice(&1u16.to_le_bytes());
        dt.push(DATA_PATTERN_NOTES);
        push_varint(&mut dt, 48);
        dt.extend_from_slice(&[1u8; 48]);
        push_utf16_event(&mut dt, TEXT_PAT_NAME, "Bassline");

        // Channel 0: Fruity Wrapper hosting Serum 2
        dt.push(WORD_NEW_CHAN);
        dt.extend_from_slice(&0u16.to_le_bytes());
        push_utf16_event(&mut dt, TEXT_PLUGIN_NAME, "Fruity Wrapper");
        push_utf16_event(&mut dt, TEXT_DISPLAY_NAME, "Lead Synth");

        let mut wrapper_payload = Vec::new();
        wrapper_payload.extend_from_slice(&10u32.to_le_bytes()); // VST3 tag
        wrapper_payload.extend_from_slice(&30u32.to_le_bytes()); // Name sub-id
        let vst_name = b"Serum 2\0";
        wrapper_payload.extend_from_slice(&(vst_name.len() as u64).to_le_bytes());
        wrapper_payload.extend_from_slice(vst_name);
        dt.push(DATA_PLUGIN_STATE);
        push_varint(&mut dt, wrapper_payload.len());
        dt.extend_from_slice(&wrapper_payload);

        // Channel 1: Sampler with WAV
        dt.push(WORD_NEW_CHAN);
        dt.extend_from_slice(&1u16.to_le_bytes());
        push_utf16_event(&mut dt, TEXT_DISPLAY_NAME, "Kick");
        push_utf16_event(&mut dt, TEXT_SAMPLE_PATH, "Samples\\Kick.wav");

        // Mixer Insert 0 (Master) with optional FX
        dt.push(DATA_INSERT_PARAMS);
        push_varint(&mut dt, 12);
        dt.extend_from_slice(&[0u8; 12]);
        if let Some(fx) = extra_fx {
            push_utf16_event(&mut dt, TEXT_PLUGIN_NAME, fx);
        }
        dt.push(WORD_SLOT_INDEX);
        dt.extend_from_slice(&0u16.to_le_bytes());

        let mut out = Vec::new();
        out.extend_from_slice(b"FLhd");
        out.extend_from_slice(&6u32.to_le_bytes());
        out.extend_from_slice(&0i16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&96u16.to_le_bytes());
        out.extend_from_slice(b"FLdt");
        out.extend_from_slice(&(dt.len() as u32).to_le_bytes());
        out.extend_from_slice(&dt);
        out
    }

    #[test]
    fn parses_synthetic_flp_with_vst_wrapper_and_mixer() {
        let bytes = build_synthetic_flp(128_000, Some("Fruity Limiter"));
        let manifest = parse_flp_bytes(&bytes).expect("should parse synthetic flp");

        assert_eq!(manifest.fl_version, "24.2.2.4597");
        assert!((manifest.bpm - 128.0).abs() < 0.001);
        assert_eq!(manifest.ppq, 96);
        assert_eq!(manifest.time_sig_num, 4);
        assert_eq!(manifest.time_sig_den, 4);

        assert_eq!(manifest.channels.len(), 2);
        assert_eq!(manifest.channels[0].name, "Lead Synth");
        assert_eq!(manifest.channels[0].effective_plugin_name(), Some("Serum 2"));
        assert_eq!(
            manifest.channels[1].sample_path.as_deref(),
            Some("Samples\\Kick.wav")
        );

        assert_eq!(manifest.patterns.len(), 1);
        assert_eq!(manifest.patterns[0].name, "Bassline");
        assert_eq!(manifest.patterns[0].note_count, 2);

        assert_eq!(manifest.mixer_inserts.len(), 1);
        assert_eq!(manifest.mixer_inserts[0].name, "Master");
        assert_eq!(manifest.mixer_inserts[0].fx_plugins, vec!["Fruity Limiter"]);
    }

    #[test]
    fn rejects_invalid_magic() {
        let err = parse_flp_bytes(b"NOT_AN_FLP_FILE_00000000").unwrap_err();
        assert!(matches!(err, FlpError::InvalidHeader));
    }

    #[test]
    fn parses_flp_inside_zipped_loop_package() {
        use std::io::Write;
        let flp_bytes = build_synthetic_flp(132_500, Some("Fruity Delay 3"));
        let zip_path = std::env::temp_dir().join("flint_test_zipped_loop.zip");
        {
            let file = File::create(&zip_path).expect("create temp zip");
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("track.flp", options).expect("start entry");
            zip.write_all(&flp_bytes).expect("write flp bytes");
            zip.finish().expect("finish zip");
        }

        let manifest = parse_flp_path(&zip_path).expect("parse flp from zip");
        let _ = std::fs::remove_file(&zip_path);

        assert!((manifest.bpm - 132.5).abs() < 0.001);
        assert_eq!(manifest.mixer_inserts[0].fx_plugins, vec!["Fruity Delay 3"]);
    }
}
