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

/// What the check adds to `error`, a record of the object store that
/// does not read, where a command that only reads opens the store all
/// the same: the record lies past where its file was last synced to,
/// among appends left unsynced in the running boot, which the check
/// reads record by record and such a command does not.
fn left_unread(alt_dir: &std::path::Path, error: Cause) -> Cause {
    let said = error.to_string();
    // a file short of what was synced is told as that
    if said.contains("ends at byte") {
        return error;
    }
    let Ok(passed) = alt_odb::NativeOdb::reading(alt_dir).passed() else {
        return error;
    };
    let stops = passed
        .iter()
        .filter(|(_, passed)| passed.holds < passed.len);
    let stops: Vec<String> = stops
        .map(|(path, passed)| {
            let file = path.strip_prefix(alt_dir).unwrap_or(path);
            format!("{} from byte {} on", file.display(), passed.holds)
        })
        .collect();
    if stops.is_empty() {
        return error;
    }
    let files = stops.join(", ");
    format!(
        "{said}\nfsck: {}",
        left_unread_note(&files, OBJECTS_WRITER, "what was staged since")
    )
    .into()
}

/// How the five append files go on past the records held of them
/// ([`super::ends`]) and, for each, from where appends were left unsynced
/// in the running boot. A file that ends short of the records its marker
/// says were synced fails this.
pub(super) fn ends(store: &mut Store) -> Res<(Vec<super::End>, Vec<Option<u64>>)> {
    let alt_dir = store.alt_dir().to_owned();
    let left_unread = |error| left_unread(&alt_dir, error);
    let synced = store.odb().check_synced().map_err(super::with_cause);
    synced.map_err(left_unread)?;
    let log = store.refs_mut().oplog().check_synced();
    log.map_err(super::with_cause)?;
    let ends = super::ends(store).map_err(left_unread)?;
    let left = store.odb().left_unsynced_from();
    let mut left = left.map_err(super::with_cause)?.to_vec();
    left.push(store.refs_mut().oplog().left_unsynced_from());
    Ok((ends, left))
}

/// What is told of `file`, which reads up to byte `holds` with a whole
/// record past that at byte `at`, where nothing says the file was never
/// synced there; `left`: from where it was left unsynced in this boot.
pub(super) fn unread(file: &str, holds: u64, at: u64, left: Option<u64>) -> Vec<String> {
    let mut told = vec![format!(
        "{file} reads up to byte {holds}, and a whole record begins past that, at byte {at}"
    )];
    if left.is_some_and(|left| holds >= left) {
        told.push(left_unread_in(file, holds));
    }
    told
}

/// The same of one file, `file`, that reads up to byte `holds`.
fn left_unread_in(file: &str, holds: u64) -> String {
    let files = format!("{file} from byte {holds} on");
    match file == LOG {
        true => left_unread_note(&files, ANY_WRITER, "an add it discarded"),
        false => left_unread_note(&files, OBJECTS_WRITER, "what was staged since"),
    }
}

/// What is said of a record that does not read among appends left
/// unsynced in the running boot, in `files`; `by` cuts it off.
fn left_unread_note(files: &str, by: &str, again: &str) -> String {
    format!(
        "that record lies past where its file was last synced to ({files}), among what was \
         written since this machine started and never confirmed as synced: nothing that was \
         synced is damaged. {by} cuts the file there and says what it discarded; {again} is \
         then to be added again"
    )
}

type Cause = Box<dyn std::error::Error>;

const ANY: &str = "the next command that writes";
const OBJECTS: &str = "the next command that writes objects";
const OBJECTS_WRITER: &str = "The next command that stores objects";
const LOG: &str = "oplog/log";
const ANY_WRITER: &str = "The next command that writes";

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
