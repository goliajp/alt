//! `alt undo` covers `alt add` too. The op log gets an
//! `PAYLOAD_INDEX_TX` entry per `add`; `undo` parses that kind, restores
//! the index entries to their prior state, and records an inverse op so
//! the second `undo` brings the staging back. Extends the ref-tx undo
//! toward "any state-changing op is reversible".

use std::path::Path;
use std::process::{Command, Output};

fn alt(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_alt"))
        .current_dir(repo)
        .env("ALT_NO_DAEMON", "1")
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("USER", "tester")
        .args(args)
        .output()
        .unwrap()
}

fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn staged_paths(repo: &Path) -> String {
    ok(alt(repo, &["status", "--json"]))
}

#[test]
fn undo_unstages_a_single_alt_add() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));

    let after_add = staged_paths(root);
    assert!(
        after_add.contains("\"a.txt\""),
        "staged set must contain a.txt after add: {after_add}"
    );

    // Undo the add — the index loses the entry; status reports the file
    // back as untracked (no head yet, so it was a pure add).
    ok(alt(root, &["undo"]));
    let after_undo = staged_paths(root);
    assert!(
        !after_undo.contains("\"staged\":[{\"path\":\"a.txt\""),
        "a.txt must drop out of the staged column after undo: {after_undo}"
    );
    assert!(
        after_undo.contains("\"untracked\":[\"a.txt\"]"),
        "a.txt re-surfaces as untracked after undo: {after_undo}"
    );
}

#[test]
fn undo_undoes_only_the_last_add_when_there_are_two() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(root.join("b.txt"), "beta\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));
    ok(alt(root, &["add", "b.txt"]));

    let after_both = staged_paths(root);
    assert!(after_both.contains("\"a.txt\"") && after_both.contains("\"b.txt\""));

    // One undo: only `b.txt` falls out; `a.txt` is still staged.
    ok(alt(root, &["undo"]));
    let after_one = staged_paths(root);
    assert!(
        after_one.contains("\"a.txt\""),
        "a.txt must still be staged after one undo: {after_one}"
    );
    assert!(
        after_one.contains("\"untracked\":[\"b.txt\"]"),
        "b.txt back to untracked after one undo: {after_one}"
    );
}

#[test]
fn undo_of_undo_redoes_the_add() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));
    ok(alt(root, &["undo"]));
    // The first undo recorded its own inverse index-tx op, so a second
    // undo replays the original add — staged again, no manual restage.
    ok(alt(root, &["undo"]));
    let after_redo = staged_paths(root);
    assert!(
        after_redo.contains("\"a.txt\""),
        "a.txt must be staged again after undo-of-undo: {after_redo}"
    );
}

#[test]
fn undo_after_commit_still_inverts_the_ref_tx_not_the_index() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    ok(alt(root, &["add", "a.txt"]));
    ok(alt(root, &["commit", "-m", "first"]));

    let index = index_bytes(root);
    ok(alt(root, &["undo"]));
    assert_eq!(index_bytes(root), index);
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"alpha\n");
    // Log should no longer find any commit on main.
    let log_out = alt(root, &["log", "--pretty=oneline"]);
    let log = String::from_utf8_lossy(&log_out.stdout);
    let stderr = String::from_utf8_lossy(&log_out.stderr);
    assert!(
        log.trim().is_empty() || stderr.contains("bad revision") || stderr.contains("unborn"),
        "after undoing the only commit, log must be empty or report unborn HEAD: stdout={log} stderr={stderr}",
    );
}

fn index_bytes(root: &Path) -> Vec<u8> {
    std::fs::read(root.join(".alt/index")).unwrap()
}

#[test]
fn recovery_rejects_cross_workspace_without_touching_either_index() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path().join("repo");
    let peer = dir.path().join("peer");
    std::fs::create_dir(&root).unwrap();
    ok(alt(&root, &["init", "."]));
    std::fs::write(root.join("f"), "baseline").unwrap();
    ok(alt(&root, &["add", "f"]));
    ok(alt(&root, &["commit", "-m", "baseline"]));
    ok(alt(
        &root,
        &["workspace", "add", "peer", peer.to_str().unwrap()],
    ));
    std::fs::write(root.join("f"), "main staged").unwrap();
    ok(alt(&root, &["add", "f"]));
    std::fs::write(peer.join("f"), "peer staged").unwrap();
    ok(alt(&peer, &["add", "f"]));
    let main_before = index_bytes(&root);
    let peer_index = root.join(".alt/workspaces/peer/index");
    let peer_before = std::fs::read(&peer_index).unwrap();
    let log = std::fs::read(root.join(".alt/oplog/log")).unwrap();
    let refused = alt(&root, &["undo"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("different workspace"));
    assert_eq!(index_bytes(&root), main_before);
    assert_eq!(std::fs::read(&peer_index).unwrap(), peer_before);
    assert_eq!(std::fs::read(root.join(".alt/oplog/log")).unwrap(), log);
    ok(alt(&peer, &["undo"]));
    assert_eq!(index_bytes(&root), main_before);
    assert_eq!(std::fs::read(peer.join("f")).unwrap(), b"peer staged");
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"main staged");
}

#[test]
fn recovery_refuses_modified_worktree_and_index_without_partial_changes() {
    use alt_git_codec::HashAlgo;
    use alt_git_index::Index;
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f"), "staged").unwrap();
    ok(alt(root, &["add", "f"]));
    let before = index_bytes(root);
    std::fs::write(root.join("f"), "later edit").unwrap();
    let refused = alt(root, &["undo"]);
    assert!(!refused.status.success());
    assert_eq!(index_bytes(root), before);
    std::fs::write(root.join("f"), "staged").unwrap();
    let mut conflict = Index::parse(&before, HashAlgo::Sha1).unwrap();
    conflict.entries[0].flags |= 1 << 12;
    let bytes = conflict.serialize(HashAlgo::Sha1);
    std::fs::write(root.join(".alt/index"), &bytes).unwrap();
    assert!(!alt(root, &["undo"]).status.success());
    assert_eq!(index_bytes(root), bytes);
}

#[test]
fn recovery_reads_frozen_legacy_record_but_refuses_automatic_undo() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    let payload = include_bytes!("fixtures/legacy-index-v1.bin");
    let changes = alt_cli::index_tx::decode(payload).unwrap();
    assert_eq!(changes.len(), 1);
    {
        let mut refs = alt_refs::RefStore::open(root.join(".alt")).unwrap();
        refs.record_op("legacy-fixture", 1, payload).unwrap();
    }
    let before = index_bytes(root);
    let log = std::fs::read(root.join(".alt/oplog/log")).unwrap();
    ok(alt(root, &["op-log", "--json"]));
    let refused = alt(root, &["undo"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("legacy operation"));
    assert_eq!(index_bytes(root), before);
    assert_eq!(std::fs::read(root.join(".alt/oplog/log")).unwrap(), log);
}

#[test]
fn commit_undo_preserves_untracked_collision_without_a_checkout() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f"), "baseline").unwrap();
    std::fs::write(root.join("keep"), "untouched").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "baseline"]));
    std::fs::remove_file(root.join("f")).unwrap();
    ok(alt(root, &["add", "f"]));
    ok(alt(root, &["commit", "-m", "remove"]));
    std::fs::write(root.join("f"), "untracked precious data").unwrap();
    let index = index_bytes(root);
    ok(alt(root, &["undo"]));
    assert_eq!(index_bytes(root), index);
    assert_eq!(
        std::fs::read(root.join("f")).unwrap(),
        b"untracked precious data"
    );
}

#[test]
fn recovery_missing_completion_survives_restart_and_blocks_mutation() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f"), "staged").unwrap();
    ok(alt(root, &["add", "f"]));
    let path = root.join(".alt/oplog/log");
    let bytes = std::fs::read(&path).unwrap();
    let mut cursor = 5;
    let mut last = 5;
    while cursor < bytes.len() {
        last = cursor;
        let n = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4 + n + 8;
    }
    // Reproduce a crash after atomic index replacement, before completion.
    std::fs::write(&path, &bytes[..last]).unwrap();
    let before = index_bytes(root);
    let log_before = std::fs::read(&path).unwrap();
    ok(alt(root, &["status", "--json"]));
    for args in [
        &["undo"][..],
        &["add", "f"][..],
        &["commit", "-m", "must not apply"][..],
    ] {
        let refused = alt(root, args);
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("outcome unknown"));
        assert_eq!(index_bytes(root), before);
        assert_eq!(std::fs::read(&path).unwrap(), log_before);
    }
}

#[test]
fn recovery_workspace_name_reuse_cannot_claim_the_old_instance() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path().join("root");
    let peer = dir.path().join("peer");
    let recreated = dir.path().join("recreated");
    std::fs::create_dir(&root).unwrap();
    ok(alt(&root, &["init", "."]));
    std::fs::write(root.join("f"), "base").unwrap();
    ok(alt(&root, &["add", "f"]));
    ok(alt(&root, &["commit", "-m", "base"]));
    ok(alt(
        &root,
        &["workspace", "add", "peer", peer.to_str().unwrap()],
    ));
    std::fs::write(peer.join("f"), "staged").unwrap();
    ok(alt(&peer, &["add", "f"]));
    let completion = {
        let log = alt_oplog::OpLog::open(&root.join(".alt/oplog")).unwrap();
        log.ops().last().unwrap().payload.clone()
    };
    let id_path = root.join(".alt/workspaces/peer/workspace-id");
    let previous_id = std::fs::read(&id_path).unwrap();
    ok(alt(&root, &["workspace", "remove", "peer"]));
    ok(alt(
        &root,
        &[
            "workspace",
            "add",
            "peer",
            recreated.to_str().unwrap(),
            "peer",
        ],
    ));
    assert_ne!(std::fs::read(&id_path).unwrap(), previous_id);
    // Select the historical completion again; its intent still names old id.
    {
        let mut refs = alt_refs::RefStore::open(root.join(".alt")).unwrap();
        refs.record_op("test-selection", 1, &completion).unwrap();
    }
    let index_path = root.join(".alt/workspaces/peer/index");
    let before = std::fs::read(&index_path).unwrap();
    assert!(!alt(&recreated, &["undo"]).status.success());
    assert_eq!(std::fs::read(&index_path).unwrap(), before);
}

#[test]
fn recovery_add_preserves_unrelated_conflict_stages_and_undo_restores_them() {
    use alt_git_codec::HashAlgo;
    use alt_git_index::Index;
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("conflict"), "conflicted").unwrap();
    ok(alt(root, &["add", "conflict"]));
    let mut index = Index::parse(&index_bytes(root), HashAlgo::Sha1).unwrap();
    index.entries[0].flags |= 1 << 12;
    std::fs::write(root.join(".alt/index"), index.serialize(HashAlgo::Sha1)).unwrap();
    std::fs::write(root.join("other"), "other").unwrap();
    ok(alt(root, &["add", "other"]));
    let staged = Index::parse(&index_bytes(root), HashAlgo::Sha1).unwrap();
    assert!(
        staged
            .entries
            .iter()
            .any(|e| e.path == b"conflict"[..] && e.stage() == 1)
    );
    ok(alt(root, &["undo"]));
    let restored = Index::parse(&index_bytes(root), HashAlgo::Sha1).unwrap();
    assert_eq!(restored.entries.len(), 1);
    assert_eq!(restored.entries[0].stage(), 1);
}

fn remove_final_log_frame(root: &Path) {
    let path = root.join(".alt/oplog/log");
    let bytes = std::fs::read(&path).unwrap();
    let mut cursor = 5;
    let mut last = 5;
    while cursor < bytes.len() {
        last = cursor;
        let len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4 + len + 8;
    }
    std::fs::write(path, &bytes[..last]).unwrap();
}

#[test]
fn recovery_ref_intent_precedes_files_and_survives_missing_completion() {
    for restore_old_file in [false, true] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        ok(alt(root, &["init", "."]));
        std::fs::write(root.join("f"), "before").unwrap();
        ok(alt(root, &["add", "f"]));
        ok(alt(root, &["commit", "-m", "base"]));
        ok(alt(root, &["branch", "before"]));
        std::fs::write(root.join("f"), "committed data").unwrap();
        ok(alt(root, &["add", "f"]));
        ok(alt(root, &["commit", "-m", "first"]));
        let before_index = index_bytes(root);
        ok(alt(root, &["switch", "before"]));
        remove_final_log_frame(root);
        if restore_old_file {
            // The switch committed its intent, but materialization never began.
            std::fs::write(root.join("f"), "committed data").unwrap();
            std::fs::write(root.join(".alt/index"), before_index).unwrap();
        }
        let before = index_bytes(root);
        let log = std::fs::read(root.join(".alt/oplog/log")).unwrap();
        let result = alt(root, &["add", "."]);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("incomplete ref recovery"));
        assert_eq!(index_bytes(root), before);
        assert_eq!(std::fs::read(root.join(".alt/oplog/log")).unwrap(), log);
        ok(alt(root, &["op-log", "--json"]));
        recover_pending(root);
        assert_eq!(std::fs::read(root.join("f")).unwrap(), b"before");
    }
}

#[test]
fn recovery_prepared_index_without_replacement_is_not_reported_as_success() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    let initial = index_bytes(root);
    std::fs::write(root.join("f"), "new").unwrap();
    ok(alt(root, &["add", "f"]));
    remove_final_log_frame(root);
    std::fs::write(root.join(".alt/index"), &initial).unwrap();
    let refused = alt(root, &["undo"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("outcome unknown"));
    assert_eq!(index_bytes(root), initial);
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"new");
    recover_pending(root);
    assert!(staged_paths(root).contains("\"staged\":[{\"path\":\"f\""));
}

fn recover_pending(root: &Path) {
    let pending: serde_json::Value =
        serde_json::from_str(&ok(alt(root, &["operation", "pending", "--json"]))).unwrap();
    let operations = pending["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 1);
    let id = operations[0]["operation_id"].as_str().unwrap();
    let before: serde_json::Value =
        serde_json::from_str(&ok(alt(root, &["operation", "status", id, "--json"]))).unwrap();
    assert_eq!(before["operation"]["outcome"], "unknown");
    let after: serde_json::Value =
        serde_json::from_str(&ok(alt(root, &["operation", "recover", id, "--json"]))).unwrap();
    assert_eq!(after["operation"]["state"], "completed");
    let log = std::fs::read(root.join(".alt/oplog/log")).unwrap();
    ok(alt(root, &["operation", "recover", id]));
    assert_eq!(
        std::fs::read(root.join(".alt/oplog/log")).unwrap(),
        log,
        "recovery retry must not append or apply again"
    );
}

#[test]
fn recovery_pending_operation_refuses_an_unrecorded_third_state() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f"), "recorded").unwrap();
    ok(alt(root, &["add", "f"]));
    remove_final_log_frame(root);
    std::fs::write(root.join("f"), "later user work").unwrap();
    let pending: serde_json::Value =
        serde_json::from_str(&ok(alt(root, &["operation", "pending", "--json"]))).unwrap();
    let id = pending["operations"][0]["operation_id"].as_str().unwrap();
    let before = index_bytes(root);
    let log = std::fs::read(root.join(".alt/oplog/log")).unwrap();
    assert!(!alt(root, &["operation", "recover", id]).status.success());
    assert_eq!(index_bytes(root), before);
    assert_eq!(std::fs::read(root.join(".alt/oplog/log")).unwrap(), log);
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"later user work");
}

#[test]
fn recovery_reads_the_frozen_legacy_frame_chain_without_rewriting_it() {
    // Captured from the pre-fix product (c355232, Linux, 2026-10-17).
    // These are the original ALTL frames, not frames generated by this test.
    let bytes = include_bytes!("fixtures/legacy-oplog-v1.bin");
    let dir = alt_os::temp::tempdir().unwrap();
    let oplog_dir = dir.path().join("oplog");
    std::fs::create_dir(&oplog_dir).unwrap();
    std::fs::write(oplog_dir.join("log"), bytes).unwrap();
    {
        let refs = alt_refs::RefStore::open(dir.path()).unwrap();
        assert!(refs.len() >= 3);
        assert_eq!(
            refs.last_op().unwrap().payload,
            include_bytes!("fixtures/legacy-index-v1.bin")
        );
        assert!(refs.oplog().ops().len() >= 6);
    }
    assert_eq!(std::fs::read(oplog_dir.join("log")).unwrap(), bytes);
}

#[test]
fn recovery_resumes_each_file_and_index_boundary_without_touching_untracked_data() {
    for boundary in 0..4 {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        ok(alt(root, &["init", "."]));
        for path in ["a", "b"] {
            std::fs::write(root.join(path), "old").unwrap();
        }
        ok(alt(root, &["add", "."]));
        ok(alt(root, &["commit", "-m", "base"]));
        ok(alt(root, &["branch", "before"]));
        for path in ["a", "b"] {
            std::fs::write(root.join(path), "new").unwrap();
        }
        ok(alt(root, &["add", "."]));
        ok(alt(root, &["commit", "-m", "change"]));
        let index_before = index_bytes(root);
        std::fs::write(root.join("untracked"), "precious").unwrap();
        ok(alt(root, &["switch", "before"]));
        remove_final_log_frame(root);
        // Model the persisted states at each boundary of the two-file checkout.
        if boundary == 0 {
            std::fs::write(root.join("a"), "new").unwrap();
        }
        if boundary <= 1 {
            std::fs::write(root.join("b"), "new").unwrap();
        }
        if boundary <= 2 {
            std::fs::write(root.join(".alt/index"), index_before).unwrap();
        }
        recover_pending(root);
        for path in ["a", "b"] {
            assert_eq!(std::fs::read(root.join(path)).unwrap(), b"old");
        }
        assert_eq!(std::fs::read(root.join("untracked")).unwrap(), b"precious");
        let status: serde_json::Value = serde_json::from_str(&staged_paths(root)).unwrap();
        assert_eq!(status["staged"], serde_json::json!([]));
        assert_eq!(status["unstaged"], serde_json::json!([]));
    }
}

#[test]
fn commit_and_amend_undo_preserve_exact_index_files_modes_links_and_later_edits() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for kind in ["first", "commit", "amend"] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        ok(alt(root, &["init", "."]));
        if kind != "first" {
            std::fs::write(root.join("f"), "baseline").unwrap();
            ok(alt(root, &["add", "."]));
            ok(alt(root, &["commit", "-m", "baseline"]));
        }
        let refs_before = alt_refs::RefStore::open(root.join(".alt"))
            .unwrap()
            .iter()
            .map(|(n, t)| (n.to_owned(), t.clone()))
            .collect::<Vec<_>>();
        std::fs::write(root.join("f"), "staged change").unwrap();
        std::fs::write(root.join("exec"), "executable").unwrap();
        std::fs::set_permissions(root.join("exec"), std::fs::Permissions::from_mode(0o751))
            .unwrap();
        symlink("f", root.join("link")).unwrap();
        ok(alt(root, &["add", "."]));
        let args = if kind == "amend" {
            vec!["commit", "--amend", "-m", "changed"]
        } else {
            vec!["commit", "-m", "changed"]
        };
        ok(alt(root, &args));
        let refs_after = alt_refs::RefStore::open(root.join(".alt"))
            .unwrap()
            .iter()
            .map(|(n, t)| (n.to_owned(), t.clone()))
            .collect::<Vec<_>>();
        std::fs::write(root.join("f"), "later unstaged edit").unwrap();
        std::fs::write(root.join("untracked"), "precious").unwrap();
        let index = index_bytes(root);
        for expected_refs in [refs_before, refs_after] {
            ok(alt(root, &["undo"]));
            assert_eq!(index_bytes(root), index, "{kind}");
            assert_eq!(
                std::fs::read(root.join("f")).unwrap(),
                b"later unstaged edit"
            );
            assert_eq!(std::fs::read(root.join("exec")).unwrap(), b"executable");
            assert_eq!(
                std::fs::metadata(root.join("exec"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o751
            );
            assert_eq!(
                std::fs::read_link(root.join("link")).unwrap(),
                Path::new("f")
            );
            assert_eq!(std::fs::read(root.join("untracked")).unwrap(), b"precious");
            let refs = alt_refs::RefStore::open(root.join(".alt"))
                .unwrap()
                .iter()
                .map(|(n, t)| (n.to_owned(), t.clone()))
                .collect::<Vec<_>>();
            assert_eq!(refs, expected_refs, "{kind}");
        }
    }
}

#[test]
fn ref_undo_refuses_legacy_missing_or_unknown_effects_without_mutation() {
    for effects in [None, Some("unknown"), Some("checkout")] {
        let dir = alt_os::temp::tempdir().unwrap();
        let root = dir.path();
        ok(alt(root, &["init", "."]));
        std::fs::write(root.join("f"), "precious").unwrap();
        ok(alt(root, &["add", "."]));
        ok(alt(root, &["commit", "-m", "base"]));
        let mut refs = alt_refs::RefStore::open(root.join(".alt")).unwrap();
        let tx = alt_refs::parse_tx(&refs.last_op().unwrap().payload)
            .unwrap()
            .unwrap();
        let mut context: serde_json::Value = serde_json::from_slice(&tx.context.unwrap()).unwrap();
        if let Some(effect) = effects {
            context["effects"] = effect.into();
        } else {
            context["version"] = 1.into();
            context.as_object_mut().unwrap().remove("effects");
        }
        let change = alt_refs::RefChange {
            name: "refs/heads/fixture".to_owned(),
            old: None,
            new: Some(alt_refs::RefTarget::Oid(
                refs.resolve("HEAD").unwrap().unwrap(),
            )),
        };
        refs.commit_scoped(
            "human:tester commit",
            1,
            &[change],
            None,
            None,
            Some(&serde_json::to_vec(&context).unwrap()),
        )
        .unwrap();
        drop(refs);
        let index = index_bytes(root);
        let log = std::fs::read(root.join(".alt/oplog/log")).unwrap();
        let result = alt(root, &["undo", "--json"]);
        assert!(!result.status.success());
        let response: serde_json::Value = serde_json::from_slice(&result.stderr).unwrap();
        assert_eq!(response["error"]["kind"], "unsupported_record");
        assert_eq!(index_bytes(root), index);
        assert_eq!(std::fs::read(root.join(".alt/oplog/log")).unwrap(), log);
        assert_eq!(std::fs::read(root.join("f")).unwrap(), b"precious");
    }
}

#[test]
fn commit_undo_request_replay_never_applies_a_second_inverse() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f"), "precious").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "first"]));
    let index = index_bytes(root);
    let args = [
        "--request-id",
        "11223344556677889900aabbccddeeff",
        "undo",
        "--json",
    ];
    ok(alt(root, &args));
    let log = std::fs::read(root.join(".alt/oplog/log")).unwrap();
    ok(alt(root, &args));
    assert_eq!(std::fs::read(root.join(".alt/oplog/log")).unwrap(), log);
    assert_eq!(index_bytes(root), index);
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"precious");
    assert!(
        alt_refs::RefStore::open(root.join(".alt"))
            .unwrap()
            .resolve("HEAD")
            .unwrap()
            .is_none()
    );
}

#[test]
fn switch_undo_checks_recorded_poststate_before_changing_any_ref() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f"), "before").unwrap();
    std::fs::write(root.join("keep"), "untouched").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    ok(alt(root, &["branch", "before"]));
    std::fs::remove_file(root.join("f")).unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "remove"]));
    ok(alt(root, &["switch", "before"]));
    ok(alt(root, &["switch", "main"]));
    std::fs::write(root.join("f"), "untracked precious").unwrap();
    let log = std::fs::read(root.join(".alt/oplog/log")).unwrap();
    let index = index_bytes(root);
    assert!(!alt(root, &["undo"]).status.success());
    assert_eq!(std::fs::read(root.join(".alt/oplog/log")).unwrap(), log);
    assert_eq!(index_bytes(root), index);
    assert_eq!(
        std::fs::read(root.join("f")).unwrap(),
        b"untracked precious"
    );
    std::fs::remove_file(root.join("f")).unwrap();
    ok(alt(root, &["undo"]));
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"before");
    ok(alt(root, &["undo"]));
    assert!(!root.join("f").exists());
}

#[test]
fn revert_undo_restores_recorded_workspace_delta() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path();
    ok(alt(root, &["init", "."]));
    std::fs::write(root.join("f"), "before").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "base"]));
    std::fs::write(root.join("f"), "after").unwrap();
    ok(alt(root, &["add", "."]));
    ok(alt(root, &["commit", "-m", "change"]));
    ok(alt(root, &["revert", "HEAD"]));
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"before");
    ok(alt(root, &["undo"]));
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"after");
    ok(alt(root, &["undo"]));
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"before");
}
