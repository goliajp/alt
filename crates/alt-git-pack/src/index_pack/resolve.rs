//! The second pass: every delta resolved to its object and hashed, by a
//! [`Walk`] from each plain entry onwards.

use alt_git_codec::{ObjectId, ObjectKind};

use super::budget::Budget;
use super::deltas::{Deltas, Of};
use super::scan::{Entry, Form, Scanned};
use super::walk::{Objects, Walk};
use super::{IndexLimits, IndexWork, MAX_DELTA_DEPTH};
use crate::{Pack, PackError};

/// The entries of a pack and the ids its objects were found to have.
struct Hashed<'a> {
    pack: &'a Pack,
    entries: &'a [Entry],
    body_end: usize,
    oids: Vec<Option<ObjectId>>,
    /// of the plain entry the walk is below
    kind: ObjectKind,
}

impl Objects for Hashed<'_> {
    type Id = ObjectId;
    type Error = PackError;

    fn payload<'a>(
        &'a mut self,
        entry: u32,
        scratch: &'a mut Vec<u8>,
    ) -> Result<&'a [u8], PackError> {
        let entry = &self.entries[entry as usize];
        (self.pack).inflate_into(entry.data_at, entry.size, self.body_end, scratch)?;
        Ok(scratch)
    }

    fn reached(&self, delta: u32) -> bool {
        self.oids[delta as usize].is_some()
    }

    fn made(&mut self, delta: u32, _: u32, _: u32, object: &[u8]) -> Result<ObjectId, PackError> {
        let oid = ObjectId::hash_object(self.pack.algo(), self.kind, object);
        self.oids[delta as usize] = Some(oid);
        Ok(oid)
    }
}

/// Gives every delta under the plain entries from `roots` on its id in
/// `oids`, which are in the order of the entries. A delta whose base is
/// no object of the pack is left without one.
pub(super) fn resolve(
    pack: &Pack,
    scanned: &mut Scanned,
    oids: &mut Vec<Option<ObjectId>>,
    roots: usize,
    limits: &IndexLimits,
    work: &mut IndexWork,
) -> Result<(), PackError> {
    let entries = &scanned.entries;
    let deltas = Deltas::new(entries.iter().map(|entry| match &entry.form {
        Form::Plain(_) => Of::Nothing,
        Form::Ofs(base) => Of::Entry(*base),
        Form::Ref(base) => Of::Object(*base),
    }));
    let budget = Budget::new(
        (limits.object_bytes, limits.inflated_bytes),
        (limits.base_bytes, limits.spill_bytes),
        MAX_DELTA_DEPTH,
        scanned.inflated,
    );
    let hashed = Hashed {
        pack,
        entries,
        body_end: scanned.body_end,
        oids: std::mem::take(oids),
        kind: ObjectKind::Blob,
    };
    let mut walk = Walk::new(hashed, &deltas, &budget);
    let mut walked = Ok(());
    for (index, entry) in entries.iter().enumerate().skip(roots) {
        if let Form::Plain(kind) = entry.form {
            let oid = walk.objects.oids[index].expect("hashed by the scan");
            walk.objects.kind = kind;
            walked = walk.below(index as u32, &oid, 0);
            if walked.is_err() {
                break;
            }
        }
    }
    *oids = walk.objects.oids;
    scanned.inflated = budget.inflated();
    work.inflated += walk.work.inflated;
    work.applied += walk.work.applied;
    work.hashed += walk.work.hashed;
    let (held, spilled) = budget.most();
    work.held = work.held.max(held);
    work.spilled = work.spilled.max(spilled);
    walked
}
