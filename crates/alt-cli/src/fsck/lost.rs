//! `alt fsck --drop-lost`: the records of the object store that name
//! content which is not stored, and taking them out.
//!
//! A command that writes cuts such records off where they were never
//! synced. Where one lies among records that were, it refuses: an older
//! alt, first to write after a power cut, recorded as synced what the cut
//! left of an `alt add` that was not. The content is gone for good, and
//! the record stands in the way of every write. This lists the records,
//! the objects that go with them and what still names those objects;
//! `--yes` takes the records out and keeps all others. An object a commit
//! names is history that is lost: its records go only with `--committed`.

use std::collections::HashSet;
use std::io::Write;

use alt_odb::NativeOdb;

use super::{FsckArgs, Res, with_cause};
use crate::native::Store;

fn kind(kind: alt_git_codec::ObjectKind) -> &'static str {
    match kind {
        alt_git_codec::ObjectKind::Blob => "blob",
        alt_git_codec::ObjectKind::Tree => "tree",
        alt_git_codec::ObjectKind::Commit => "commit",
        alt_git_codec::ObjectKind::Tag => "tag",
    }
}

pub(super) fn run<W: Write>(store: &mut Store, args: &FsckArgs, out: &mut W) -> Res<u8> {
    let alt_dir = store.alt_dir().to_owned();
    // before anything of the store is opened: a removal that was
    // interrupted can have left a record that does not read
    if NativeOdb::finish_removal(&alt_dir).map_err(with_cause)? {
        writeln!(
            out,
            "finished taking out the records an earlier run was interrupted at"
        )?;
    }
    let mut odb = NativeOdb::deferred(&alt_dir);
    let lost = odb.lost().map_err(with_cause)?;
    if lost.is_empty() {
        writeln!(out, "no record names content that is not stored")?;
        return Ok(0);
    }
    for (file, at) in &lost.records {
        writeln!(
            out,
            "the record at byte {at} of {file} names content that is not stored"
        )?;
    }
    for (oid, of) in &lost.objects {
        writeln!(
            out,
            "{} {oid} goes with them: no other record names it",
            kind(*of)
        )?;
    }
    let gone: HashSet<_> = lost.objects.iter().map(|(oid, _)| *oid).collect();
    let (staged, _) = store
        .index_findings(&gone)
        .map_err(|error| format!("fsck: {error}"))?;
    for line in &staged {
        writeln!(out, "{line}")?;
    }
    let committed = store.reaching(0, &|oid| Ok(gone.contains(oid)))?;
    for named in &committed {
        writeln!(out, "{} {}: {}", named.kind, named.oid, named.by)?;
    }
    let records = lost.records.len();
    if !args.yes {
        writeln!(
            out,
            "nothing was changed: `alt fsck --drop-lost --yes` takes these {records} record(s) out"
        )?;
        return Ok(1);
    }
    if !committed.is_empty() && !args.committed {
        return Err(format!(
            "fsck: {} object(s) that commits name go with these records, and nothing was \
             changed. Taking the records out does not bring the objects back: fetch them from \
             another copy of the repository if there is one. `alt fsck --drop-lost --yes \
             --committed` takes the records out all the same; the commits listed above then \
             stay as they are and name objects that are not stored",
            committed.len()
        )
        .into());
    }
    if !committed.is_empty() {
        store.keep_committed_lost(&gone)?;
    }
    if !crate::daemon_meta::stop(&alt_dir) {
        return Err(
            "fsck: a daemon serves this repository and did not stop: stop it, then \
                    run the command again"
                .into(),
        );
    }
    if !odb.drop_lost(&lost).map_err(with_cause)? {
        return Err(
            "fsck: the object store changed while its records were listed, and \
                    nothing was taken out: run the command again"
                .into(),
        );
    }
    writeln!(
        out,
        "took {records} record(s) out of the object store; every other record is kept"
    )?;
    if !staged.is_empty() {
        writeln!(out, "add the files named above again")?;
    }
    Ok(0)
}
