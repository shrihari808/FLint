//! Two- and three-peer integration tests.
//!
//! The default tests run in `NetworkMode::LocalOnly` (loopback, no relays, no DNS) so they pass
//! offline and in CI. `two_peer_sync_over_internet` runs the same scenario through n0's public
//! relays and address lookup; run it on purpose with:
//!
//!     cargo test -p flint-sync --test p2p_sync -- --ignored

use flint_core::{Comment, PeerEnvironment};
use flint_sync::{
    read_workspace_record, NetworkMode, NodeOptions, ParentSource, PullOptions, PushOptions,
    ShareOptions, SyncNode,
};
use std::io::Write;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

fn build_test_flp(bpm_milli: u32, samples: &[&str]) -> Vec<u8> {
    fn push_varint(out: &mut Vec<u8>, mut len: usize) {
        loop {
            let mut b = (len & 0x7F) as u8;
            len >>= 7;
            if len > 0 {
                b |= 0x80;
            }
            out.push(b);
            if len == 0 {
                break;
            }
        }
    }
    fn push_utf16(out: &mut Vec<u8>, id: u8, s: &str) {
        let mut raw = Vec::new();
        for u in s.encode_utf16() {
            raw.extend_from_slice(&u.to_le_bytes());
        }
        raw.extend_from_slice(&0u16.to_le_bytes());
        out.push(id);
        push_varint(out, raw.len());
        out.extend_from_slice(&raw);
    }

    let mut dt = Vec::new();
    dt.push(199);
    let ver = b"24.2.2.4597\0";
    push_varint(&mut dt, ver.len());
    dt.extend_from_slice(ver);
    dt.push(156);
    dt.extend_from_slice(&bpm_milli.to_le_bytes());
    dt.extend_from_slice(&[17, 4, 18, 4]);
    for (i, sample) in samples.iter().enumerate() {
        dt.push(64);
        dt.extend_from_slice(&(i as u16).to_le_bytes());
        push_utf16(&mut dt, 203, &format!("Ch{i}"));
        push_utf16(&mut dt, 196, sample);
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"FLhd");
    out.extend_from_slice(&6u32.to_le_bytes());
    out.extend_from_slice(&0i16.to_le_bytes());
    out.extend_from_slice(&(samples.len() as u16).to_le_bytes());
    out.extend_from_slice(&96u16.to_le_bytes());
    out.extend_from_slice(b"FLdt");
    out.extend_from_slice(&(dt.len() as u32).to_le_bytes());
    out.extend_from_slice(&dt);
    out
}

async fn start(dir: &Path, network: NetworkMode) -> SyncNode {
    let node = SyncNode::start_with_options(
        dir,
        NodeOptions {
            network,
            gc_interval: Duration::from_secs(1),
        },
    )
    .await
    .expect("start node");
    node.wait_online(Duration::from_secs(10)).await;
    node
}

/// Re-evaluate `$cond` (which may use `.await`) every 250 ms until it is true or `$secs` elapse.
/// A macro rather than a closure-taking fn so the condition can borrow local variables freely.
macro_rules! wait_until {
    ($secs:expr, $cond:expr) => {{
        let deadline = tokio::time::Instant::now() + Duration::from_secs($secs);
        let mut ok = false;
        while tokio::time::Instant::now() < deadline {
            if $cond {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        ok
    }};
}

fn push_opts(note: &str, author: &str) -> PushOptions {
    PushOptions {
        note: note.into(),
        author_display_name: author.into(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_peer_sync_local() {
    two_peer_scenario(NetworkMode::LocalOnly).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs internet access (n0 relays + DNS); run with --ignored"]
async fn two_peer_sync_over_internet() {
    two_peer_scenario(NetworkMode::Internet).await;
}

async fn two_peer_scenario(network: NetworkMode) {
    let base = std::env::temp_dir().join(format!("flint_p2p_test_{}", ulid::Ulid::new()));
    let proj_a = base.join("project_a");
    let pull_b = base.join("pull_b");
    let pull_a = base.join("pull_a");

    std::fs::create_dir_all(proj_a.join("Samples")).unwrap();
    let kick = b"RIFF_FAKE_WAV_KICK_PAYLOAD_1234567890".to_vec();
    std::fs::write(proj_a.join("Samples").join("Kick.wav"), &kick).unwrap();
    let samples = [r"Samples\Kick.wav", r"Samples\NotOnDisk.wav"];
    let flp_a = proj_a.join("track.flp");
    std::fs::write(&flp_a, build_test_flp(124_000, &samples)).unwrap();

    let node_a = start(&base.join("peer_a"), network).await;
    let node_b = start(&base.join("peer_b"), network).await;
    if network == NetworkMode::LocalOnly {
        // No DNS lookup in local mode: tell each node where the other one is.
        node_a.add_peer_addr(node_b.endpoint_addr());
        node_b.add_peer_addr(node_a.endpoint_addr());
    }

    // --- A creates the session and pushes v1 ------------------------------------------------
    let created = node_a.create_session("Collab", "Shri").await.expect("create");
    let sid = created.session_id.clone();
    node_a
        .publish_peer_env(
            &sid,
            &PeerEnvironment {
                node_id: node_a.node_id(),
                display_name: "Shri".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let v1 = node_a
        .push_project(&sid, &flp_a, push_opts("initial", "Shri"))
        .await
        .expect("push v1");
    assert_eq!(v1.new_blobs, 2, ".flp + Kick.wav");
    assert_eq!(v1.parent_source, ParentSource::None);
    // Bug 4: the missing sample is reported instead of silently skipped.
    assert_eq!(v1.missing_samples, vec![r"Samples\NotOnDisk.wav".to_string()]);
    assert_eq!(v1.snapshot.missing_samples.len(), 1);

    // --- A pushes v2 (BPM only) — Kick.wav is deduplicated ------------------------------------
    std::fs::write(&flp_a, build_test_flp(128_000, &samples)).unwrap();
    let v2 = node_a
        .push_project(&sid, &flp_a, push_opts("tempo", "Shri"))
        .await
        .expect("push v2");
    assert_eq!((v2.new_blobs, v2.reused_blobs), (1, 1));
    // Bug 1: the parent comes from the folder's .flint record (written by push v1).
    assert_eq!(v2.parent_source, ParentSource::Workspace);
    assert_eq!(v2.snapshot.parent_ids, vec![v1.snapshot.id.clone()]);
    assert!(v2.snapshot.diff_summary.iter().any(|l| l.contains("BPM: 124 → 128")));

    // --- B joins; metadata syncs, files do NOT until they are needed --------------------------
    assert_eq!(node_b.join_session(&created.invite_ticket).await.unwrap(), sid);
    assert!(
        wait_until!(30, node_b.list_snapshots(&sid).await.unwrap_or_default().len() >= 2),
        "B never received the snapshot records"
    );
    let kick_hash = v2.snapshot.assets.values().next().unwrap().clone();
    let kick_hash = iroh_blobs::Hash::from_str(&kick_hash).unwrap();
    assert!(
        !node_b.blobs().blobs().has(kick_hash).await.unwrap(),
        "files should only download on pull/prefetch, not with the metadata"
    );

    // --- B pulls v2: files are downloaded from A (bug 2) and paths rewritten (bug 5) ----------
    let pulled = node_b
        .pull_snapshot(
            &sid,
            &pull_b,
            PullOptions {
                wait: Duration::from_secs(30),
                ..Default::default()
            },
        )
        .await
        .expect("pull v2 on B");
    assert_eq!(pulled.snapshot.id, v2.snapshot.id);
    assert_eq!(pulled.downloaded_blobs, 2);
    assert_eq!(pulled.files_written, 2);
    let pulled_kick = pulled.output_dir.join("Samples").join("Kick.wav");
    assert_eq!(std::fs::read(&pulled_kick).unwrap(), kick);
    assert_eq!(pulled.rewritten_sample_paths, 1);
    let pulled_manifest = flint_flp::parse_flp_path(&pulled.flp_path).unwrap();
    let kick_abs = std::path::absolute(&pulled_kick).unwrap();
    assert!(
        pulled_manifest
            .samples
            .contains(&kick_abs.to_string_lossy().to_string()),
        "pulled .flp should point at the pulled Kick.wav: {:?}",
        pulled_manifest.samples
    );
    assert!(pulled.original_flp_path.as_ref().unwrap().exists());
    assert_eq!(
        read_workspace_record(&pulled.output_dir, &sid).unwrap().base_snapshot_id,
        v2.snapshot.id
    );

    // Pulling again never overwrites the folder you may be editing.
    let again = node_b
        .pull_snapshot(&sid, &pull_b, PullOptions::default())
        .await
        .unwrap();
    assert_ne!(again.output_dir, pulled.output_dir);

    // --- Meanwhile A pushes v3 --------------------------------------------------------------
    std::fs::write(&flp_a, build_test_flp(129_000, &samples)).unwrap();
    let v3 = node_a
        .push_project(&sid, &flp_a, push_opts("129", "Shri"))
        .await
        .expect("push v3");
    assert_eq!(v3.snapshot.parent_ids, vec![v2.snapshot.id.clone()]);
    assert!(
        wait_until!(30, node_b.list_snapshots(&sid).await.unwrap_or_default().len() >= 3),
        "B never received v3"
    );

    // --- B edits its pulled v2 and pushes: parent must be v2, not v3 (bug 1) ------------------
    std::fs::write(
        &pulled.flp_path,
        build_test_flp(130_000, &[&kick_abs.to_string_lossy(), r"Samples\NotOnDisk.wav"]),
    )
    .unwrap();
    let v4 = node_b
        .push_project(&sid, &pulled.flp_path, push_opts("130 from Alex", "Alex"))
        .await
        .expect("push v4 from B");
    assert_eq!(v4.parent_source, ParentSource::Workspace);
    assert_eq!(v4.snapshot.parent_ids, vec![v2.snapshot.id.clone()]);
    assert!(v4.diverged_from_latest, "A pushed v3 after B's base");
    assert!(
        v4.snapshot.diff_summary.iter().any(|l| l.contains("BPM: 128 → 130")),
        "diff must be against B's base (v2), got {:?}",
        v4.snapshot.diff_summary
    );
    assert_eq!((v4.new_blobs, v4.reused_blobs), (1, 1));

    let comment_id = ulid::Ulid::new().to_string();
    node_b
        .upsert_comment(
            &sid,
            &Comment {
                id: comment_id.clone(),
                snapshot_id: v4.snapshot.id.clone(),
                author: "Alex".into(),
                bar: 17,
                beat: 1.0,
                time_sec: 29.5,
                text: "Drop hits harder at 130".into(),
                resolved: false,
                created_at: 2,
            },
        )
        .await
        .unwrap();

    assert!(
        wait_until!(
            30,
            node_a.list_snapshots(&sid).await.unwrap_or_default().len() >= 4
                && !node_a.list_comments(&sid, None).await.unwrap_or_default().is_empty()
        ),
        "A did not receive B's snapshot and comment"
    );

    // --- A pulls B's v4: only the new .flp has to travel ------------------------------------
    let a_pull = node_a
        .pull_snapshot(
            &sid,
            &pull_a,
            PullOptions {
                snapshot_id: Some(v4.snapshot.id.clone()),
                wait: Duration::from_secs(30),
                ..Default::default()
            },
        )
        .await
        .expect("A pulls v4");
    assert_eq!(a_pull.downloaded_blobs, 1);

    // --- Prune on A keeps only v4's files (bug 7) -------------------------------------------
    let dry = node_a.prune(&sid, 1, true).await.unwrap();
    assert_eq!(dry.released_blobs, 3, "v1, v2, v3 .flp files: {dry:?}");
    let pruned = node_a.prune(&sid, 1, false).await.unwrap();
    assert_eq!(pruned.released_blobs, 3);
    let v1_flp = iroh_blobs::Hash::from_str(&v1.snapshot.flp_blob).unwrap();
    assert!(
        wait_until!(15, !node_a.blobs().blobs().has(v1_flp).await.unwrap_or(true)),
        "garbage collection should delete the released v1 .flp"
    );
    // Kept files and all history survive.
    assert!(node_a.blobs().blobs().has(kick_hash).await.unwrap());
    assert_eq!(node_a.list_snapshots(&sid).await.unwrap().len(), 4);

    // --- Read-only ticket (bug 8): can read, cannot write ------------------------------------
    let read_ticket = node_a
        .share_session(&sid, ShareOptions { read_only: true, hide_addresses: false })
        .await
        .unwrap();
    let node_c = start(&base.join("peer_c"), network).await;
    if network == NetworkMode::LocalOnly {
        node_c.add_peer_addr(node_a.endpoint_addr());
        node_c.add_peer_addr(node_b.endpoint_addr());
    }
    let (sid_c, _) = node_c.join_session_read_only(&read_ticket).await.unwrap();
    assert_eq!(sid_c, sid);
    assert!(
        wait_until!(30, node_c.list_snapshots(&sid).await.unwrap_or_default().len() >= 4),
        "read-only peer should still receive history"
    );
    assert!(
        node_c
            .publish_peer_env(&sid, &PeerEnvironment::default())
            .await
            .is_err(),
        "read-only peer must not be able to write"
    );

    // --- Rotate (bug 9): history moves to a fresh namespace ----------------------------------
    let rotated = node_a.rotate_session(&sid, "Shri").await.unwrap();
    assert_ne!(rotated.created.session_id, sid);
    let new_snaps = node_a.list_snapshots(&rotated.created.session_id).await.unwrap();
    assert_eq!(new_snaps.len(), 4);
    assert_eq!(node_a.list_comments(&rotated.created.session_id, None).await.unwrap().len(), 1);

    node_c.shutdown().await.unwrap();
    node_a.shutdown().await.unwrap();
    node_b.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&base);
}

/// Bug 3: two files with the same name in different zip folders must both survive.
#[tokio::test(flavor = "multi_thread")]
async fn zip_keeps_folder_structure() {
    let base = std::env::temp_dir().join(format!("flint_zip_test_{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&base).unwrap();
    let zip_path = base.join("loop.zip");
    {
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("song.flp", opts).unwrap();
        zip.write_all(&build_test_flp(140_000, &[r"C:\Drums\kick.wav"])).unwrap();
        zip.start_file("Drums/kick.wav", opts).unwrap();
        zip.write_all(b"drum kick").unwrap();
        zip.start_file("Bass/kick.wav", opts).unwrap();
        zip.write_all(b"bass kick").unwrap();
        zip.finish().unwrap();
    }

    let node = start(&base.join("node"), NetworkMode::LocalOnly).await;
    let created = node.create_session("Zip", "Shri").await.unwrap();
    let pushed = node
        .push_project(&created.session_id, &zip_path, push_opts("zip", "Shri"))
        .await
        .unwrap();
    let keys: Vec<&String> = pushed.snapshot.assets.keys().collect();
    assert_eq!(keys, vec!["Bass/kick.wav", "Drums/kick.wav"]);
    assert!(pushed.missing_samples.is_empty());

    let pulled = node
        .pull_snapshot(&created.session_id, base.join("out"), PullOptions::default())
        .await
        .unwrap();
    assert_eq!(std::fs::read(pulled.output_dir.join("Drums/kick.wav")).unwrap(), b"drum kick");
    assert_eq!(std::fs::read(pulled.output_dir.join("Bass/kick.wav")).unwrap(), b"bass kick");

    node.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&base);
}
