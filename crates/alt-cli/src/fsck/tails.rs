//! An unconfirmed tail: what lies past the boundary a file was last
//! recorded as synced to and holds whole records that a command which
//! writes takes away. Either a record there does not read and whole
//! records follow it, or records and operations there name content that
//! is not stored.
//!
//! Nothing past that boundary was ever confirmed to the command that
//! wrote it, so a command that writes cuts the file where it stops
//! reading, and with it the records and operations that name what went.
//! That is not damage, and it is not nothing either: the check says what
//! goes, file by file, which command takes it, and exits with [`EXIT`] so
//! that whoever wants the bytes can copy `.alt` first.

use alt_json::json;

use super::{Found, Res};
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

const ANY: &str = "the next command that writes";
const OBJECTS: &str = "the next command that writes objects";
const LOG: &str = "oplog/log";

/// Which command cuts the end off `file`. The operation log's goes with
/// the next command that writes. So does the object store's when
/// operations lie past the log's boundary (`settles`): a command then
/// settles the object store to judge them before it writes anything. With
/// none there, the object store is cut by the next command that stores an
/// object; one that writes only a ref leaves it.
pub(super) fn cutter(file: &str, settles: bool) -> &'static str {
    match settles || file == LOG {
        true => ANY,
        false => OBJECTS,
    }
}

/// What goes off the five files, and which command takes it.
pub(super) struct Tail {
    discards: Discards,
    settles: bool,
}

impl Tail {
    /// The bytes that go off each file with the command `by`.
    fn files(&self, by: &'static str) -> impl Iterator<Item = &(String, u64)> {
        let cut = move |(file, _): &&(String, u64)| cutter(file, self.settles) == by;
        self.discards.files.iter().filter(cut)
    }

    fn bytes(&self, by: &'static str) -> u64 {
        self.files(by).map(|(_, bytes)| bytes).sum()
    }

    /// Each file with its bytes and the command that cuts it.
    fn each<T>(&self, entry: impl Fn(&str, u64, &'static str) -> T) -> Vec<T> {
        let files = self.discards.files.iter();
        files
            .map(|(file, bytes)| entry(file, *bytes, cut_by(cutter(file, self.settles))))
            .collect()
    }
}

/// How a command that cuts is named to a program.
fn cut_by(by: &str) -> &'static str {
    match by == ANY {
        true => "next_write",
        false => "next_object_write",
    }
}

/// What the commands that write next cut off the five files, from what
/// the check read of them.
pub(super) fn discards(
    store: &mut Store,
    found: &Found,
    unconfirmed: Option<alt_refs::Unconfirmed>,
) -> Res<Tail> {
    let mut discards = Discards::default();
    let alt_dir = store.alt_dir().to_owned();
    // records that name content which is not stored go with the pack's end
    let named = [0].into_iter().chain(found.odb.unconfirmed);
    for ((path, passed, _), named) in found.ends.iter().zip(named) {
        discards.file(&alt_dir, path, passed.len - passed.holds + named);
    }
    let (path, passed, _) = &found.ends[4];
    let from = unconfirmed.map_or(passed.holds, |found| found.at);
    discards.file(&alt_dir, path, passed.len - from);
    let log = store.refs_mut().oplog();
    let ops = log.ops()?;
    let cut = unconfirmed.map_or(0, |found| found.ops);
    discards.named = ops[ops.len() - cut..].to_vec();
    discards.after = log.passed_ops()?;
    Ok(Tail {
        discards,
        settles: found.settles,
    })
}

/// The lines that say what goes, with which command, and how to keep it.
pub(super) fn told(tail: &Tail) -> Vec<String> {
    let mut lines = Vec::new();
    let file = |(file, bytes): &(String, u64)| format!("{bytes:>12} byte(s) at the end of {file}");
    let ops = tail.discards.ops();
    if ops > 0 || tail.bytes(ANY) > 0 {
        lines.push(format!("{ANY} discards:"));
        lines.extend(tail.files(ANY).map(file));
        if ops > 0 {
            lines.push(format!(
                "{ops:>12} operation(s) at the end of the operation log"
            ));
        }
        for line in tail.discards.moves() {
            lines.push(format!("{:>12} {line}", ""));
        }
    }
    if tail.bytes(OBJECTS) > 0 {
        lines.push(format!("{OBJECTS} discards:"));
        lines.extend(tail.files(OBJECTS).map(file));
    }
    lines.push(
        "to keep them, copy the .alt directory before you run a command that writes".to_owned(),
    );
    lines
}

pub(super) fn json(tail: &Tail) -> Json {
    let files = tail.each(|file, bytes, by| {
        Json::Object(vec![
            ("file", Json::str(file)),
            ("bytes", Json::Num(bytes as i64)),
            ("cut_by", Json::str(by)),
        ])
    });
    let moves = tail.discards.moves().into_iter().map(Json::str);
    Json::Object(vec![
        ("files", Json::Array(files)),
        ("operations", Json::Num(tail.discards.ops() as i64)),
        ("refs", Json::Array(moves.collect())),
    ])
}

/// How the check ends when all it found is an unconfirmed tail.
pub(super) fn failure(tail: &Tail) -> Failure {
    let ops = tail.discards.ops();
    let (any, objects) = (tail.bytes(ANY), tail.bytes(OBJECTS));
    let first = match ops {
        0 => format!("{ANY} discards {any} byte(s)"),
        _ => format!("{ANY} discards {any} byte(s) and {ops} operation(s)"),
    };
    let goes = match (ops > 0 || any > 0, objects > 0) {
        (true, false) => first,
        (false, _) => format!("{OBJECTS} discards {objects} byte(s)"),
        (true, true) => format!("{first}, and {OBJECTS} discards {objects} byte(s),"),
    };
    let files = tail.each(|file, bytes, by| json!({"file": file, "bytes": bytes, "cut_by": by}));
    Failure {
        exit_code: EXIT,
        details: Box::new(json!({"files": files, "operations": ops})),
        next_action: "Copy the .alt directory before running a command that writes, to keep \
                      what was never confirmed as synced",
        ..Failure::refusal(
            "unconfirmed_tail",
            format!(
                "fsck: unconfirmed tail: {goes} that were never confirmed as synced; to keep \
                 them, copy the .alt directory first"
            ),
        )
    }
}
