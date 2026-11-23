use flint_core::{Comment, PeerEnvironment};
use flint_sync::SyncNode;
use std::time::Duration;

fn build_test_flp(bpm_milli: u32, sample_rel: &str) -> Vec<u8> {
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
    dt.push(64);
    dt.extend_from_slice(&0u16.to_le_bytes());
    push_utf16(&mut dt, 203, "Kick");
    push_utf16(&mut dt, 196, sample_rel);

    let mut out = Vec::new();
    out.extend_from_slice(b"FLhd");
    out.extend_from_slice(&6u32.to_le_bytes());
    out.extend_from_slice(&0i16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&96u16.to_le_bytes());
    out.extend_from_slice(b"FLdt");
    out.extend_from_slice(&(dt.len() as u32).to_le_bytes());
    out.extend_from_slice(&dt);
    out
}

#[tokio::test]
async fn two_peer_multi_writer_deduplicated_p2p_sync() {
    let base = std::env::temp_dir().join(format!("flint_p2p_test_{}", ulid::Ulid::new()));
    let dir_a = base.join("peer_a");
    let dir_b = base.join("peer_b");
    let proj_a = base.join("project_a");
    let pull_b = base.join("pull_b");

    std::fs::create_dir_all(proj_a.join("Samples")).unwrap();
    let sample_bytes = b"RIFF_FAKE_WAV_KICK_PAYLOAD_1234567890".to_vec();
    std::fs::write(proj_a.join("Samples").join("Kick.wav"), &sample_bytes).unwrap();

    let flp_a = proj_a.join("track.flp");
    std::fs::write(&flp_a, build_test_flp(124_000, r"Samples\Kick.wav")).unwrap();

    // 1. Start two independent persistent Iroh nodes
    let node_a = SyncNode::start(&dir_a).await.expect("start node A");
    let node_b = SyncNode::start(&dir_b).await.expect("start node B");
    node_a.wait_online(Duration::from_secs(5)).await;
    node_b.wait_online(Duration::from_secs(5)).await;

    // 2. Machine A creates session & pushes v1
    let created = node_a
        .create_session("Melodic Techno Collab", "Shri")
        .await
        .expect("create session");
    let sid = created.session_id.clone();

    node_a
        .publish_peer_env(
            &sid,
            &PeerEnvironment {
                node_id: node_a.node_id(),
                display_name: "Shri".into(),
                fl_version: Some("24.2.2".into()),
                installed_plugins: vec!["Serum2".into()],
                updated_at: 1,
            },
        )
        .await
        .expect("publish peer A env");

    let push1 = node_a
        .push_project(&sid, &flp_a, "initial groove", "Shri", None, vec![])
        .await
        .expect("push v1");
    assert_eq!(push1.new_blobs, 2, "v1 should ingest .flp + Kick.wav");
    assert_eq!(push1.reused_blobs, 0);

    // 3. Machine A changes ONLY BPM (124 -> 128) and pushes v2 -> Kick.wav MUST be deduplicated!
    std::fs::write(&flp_a, build_test_flp(128_000, r"Samples\Kick.wav")).unwrap();
    let push2 = node_a
        .push_project(&sid, &flp_a, "bumped tempo", "Shri", None, vec![])
        .await
        .expect("push v2");
    assert_eq!(push2.new_blobs, 1, "v2 should only ingest modified .flp");
    assert_eq!(push2.reused_blobs, 1, "v2 should reuse Kick.wav blob");
    assert!(
        push2
            .snapshot
            .diff_summary
            .iter()
            .any(|l| l.contains("BPM: 124 → 128")),
        "expected BPM diff, got {:?}",
        push2.snapshot.diff_summary
    );

    // 4. Machine B joins via invite ticket & syncs over live Iroh P2P
    let joined_sid = node_b
        .join_session(&created.invite_ticket)
        .await
        .expect("join session");
    assert_eq!(joined_sid, sid);

    // Wait for B to receive both snapshots & their blobs
    let mut synced = false;
    for _ in 0..40 {
        if let Ok(snaps) = node_b.list_snapshots(&sid).await {
            if snaps.len() >= 2 {
                if node_b.pull_snapshot(&sid, None, &pull_b).await.is_ok() {
                    synced = true;
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(synced, "Machine B failed to sync snapshots + blobs from Machine A");

    let pulled = node_b
        .pull_snapshot(&sid, None, &pull_b)
        .await
        .expect("pull latest on B");
    assert_eq!(pulled.snapshot.note, "bumped tempo");
    assert_eq!(pulled.files_written, 2);
    assert_eq!(
        std::fs::read(pulled.output_dir.join("Samples").join("Kick.wav")).unwrap(),
        sample_bytes
    );

    // 5. Multi-writer check: Machine B pushes v3 + adds a bar-locked comment -> Machine A receives both!
    std::fs::write(&pulled.flp_path, build_test_flp(130_000, r"Samples\Kick.wav")).unwrap();
    let push3 = node_b
        .push_project(&sid, &pulled.flp_path, "friend raised to 130", "Alex", None, vec![])
        .await
        .expect("multi-writer push v3 from B");
    assert_eq!(push3.new_blobs, 1);
    assert_eq!(push3.reused_blobs, 1);

    node_b
        .upsert_comment(
            &sid,
            &Comment {
                id: ulid::Ulid::new().to_string(),
                snapshot_id: push3.snapshot.id.clone(),
                author: "Alex".into(),
                bar: 17,
                beat: 1.0,
                time_sec: 29.5,
                text: "Drop hits way harder at 130!".into(),
                resolved: false,
                created_at: 2,
            },
        )
        .await
        .expect("add comment from B");

    let mut a_got_b = false;
    for _ in 0..40 {
        let snaps_a = node_a.list_snapshots(&sid).await.unwrap_or_default();
        let comments_a = node_a.list_comments(&sid, None).await.unwrap_or_default();
        if snaps_a.len() >= 3 && !comments_a.is_empty() {
            a_got_b = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(a_got_b, "Machine A did not receive Machine B's multi-writer snapshot & comment");

    node_a.shutdown().await.unwrap();
    node_b.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&base);
}
