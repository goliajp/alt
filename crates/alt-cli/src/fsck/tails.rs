//! An unconfirmed tail: bytes past the boundary a file was last recorded
//! as synced to, where a record does not read and whole records follow
//! it.
//!
//! Nothing past that boundary was ever confirmed to the command that
//! wrote it, so the next command that writes cuts the file where it stops
//! reading, and with it the records and operations that name what went.
//! That is not damage, and it is not nothing either: the check says what
//! goes, file by file, and exits with [`EXIT`] so that whoever wants the
//! bytes can copy `.alt` first.

use alt_json::json;

use super::{End, Res};
use crate::json::Json;
use crate::machine::Failure;
use crate::native::{Discards, Store};

/// The exit code of a check that found no damage and an unconfirmed tail.
pub const EXIT: u8 = 3;

/// The line for a file that ends in an unconfirmed tail.
pub(super) fn found(file: &str, holds: u64, bytes: u64, whole_at: u64) -> String {
    format!(
        "unconfirmed tail: {file} reads up to byte {holds}; the {bytes} byte(s) after it were \
         never confirmed as synced, the record at byte {holds} does not read, and a whole \
         record begins at byte {whole_at}"
    )
}

/// What the next command that writes cuts off the five files, from what
/// the check read of them.
pub(super) fn discards(
    store: &mut Store,
    ends: &[End],
    odb: &alt_odb::OdbChecked,
    unconfirmed: Option<alt_refs::Unconfirmed>,
) -> Res<Discards> {
    let mut discards = Discards::default();
    let alt_dir = store.alt_dir().to_owned();
    // records that name content which is not stored go with the pack's end
    let named = [
        0,
        odb.unconfirmed[0],
        odb.unconfirmed[1],
        odb.unconfirmed[2],
    ];
    for ((path, passed, _), named) in ends.iter().zip(named) {
        discards.file(&alt_dir, path, passed.len - passed.holds + named);
    }
    let (path, passed, _) = &ends[4];
    let from = unconfirmed.map_or(passed.holds, |found| found.at);
    discards.file(&alt_dir, path, passed.len - from);
    let log = store.refs_mut().oplog();
    let ops = log.ops()?;
    let cut = unconfirmed.map_or(0, |found| found.ops);
    discards.named = ops[ops.len() - cut..].to_vec();
    discards.after = log.passed_ops()?;
    Ok(discards)
}

/// The lines that say what goes and how to keep it.
pub(super) fn told(discards: &Discards) -> Vec<String> {
    let mut lines = vec!["the next command that writes discards:".to_owned()];
    for (file, bytes) in &discards.files {
        lines.push(format!("{bytes:>12} byte(s) at the end of {file}"));
    }
    let ops = discards.ops();
    lines.push(format!(
        "{ops:>12} operation(s) at the end of the operation log"
    ));
    for line in discards.moves() {
        lines.push(format!("{:>12} {line}", ""));
    }
    lines.push(
        "to keep them, copy the .alt directory before you run a command that writes".to_owned(),
    );
    lines
}

pub(super) fn json(discards: &Discards) -> Json {
    let files = discards.files.iter().map(|(file, bytes)| {
        Json::Object(vec![
            ("file", Json::str(file)),
            ("bytes", Json::Num(*bytes as i64)),
        ])
    });
    let moves = discards.moves().into_iter().map(Json::str);
    Json::Object(vec![
        ("files", Json::Array(files.collect())),
        ("operations", Json::Num(discards.ops() as i64)),
        ("refs", Json::Array(moves.collect())),
    ])
}

/// How the check ends when all it found is an unconfirmed tail.
pub(super) fn failure(discards: &Discards) -> Failure {
    let bytes: u64 = discards.files.iter().map(|(_, bytes)| bytes).sum();
    let ops = discards.ops();
    let files: Vec<_> = discards
        .files
        .iter()
        .map(|(file, bytes)| json!({"file": file, "bytes": bytes}))
        .collect();
    Failure {
        exit_code: EXIT,
        details: Box::new(json!({"files": files, "operations": ops})),
        next_action: "Copy the .alt directory to keep the unconfirmed tail; the next command \
                      that writes discards it",
        ..Failure::refusal(
            "unconfirmed_tail",
            format!(
                "fsck: unconfirmed tail: the next command that writes discards {bytes} byte(s) \
                 and {ops} operation(s) that were never confirmed as synced; to keep them, \
                 copy the .alt directory first"
            ),
        )
    }
}
