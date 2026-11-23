//! The objects of a push in the odb, until its refs moved.

use alt_git_codec::ObjectId;
use alt_hash::KeyedIdSet;
use alt_odb::{NativeOdb, OdbCheckpoint};

use super::{PreparedPack, Res};

/// A batch of objects that goes again unless it is kept, also when the
/// thread that wrote it panics: `odb.lock` is let go of either way.
pub(super) struct Batch<'a> {
    odb: &'a mut NativeOdb,
    begun: OdbCheckpoint,
    wrote: bool,
    kept: bool,
    ended: bool,
}

impl<'a> Batch<'a> {
    /// Takes `odb.lock`; a rollback cuts back to where the odb ends once
    /// this handle has read what any other process wrote.
    pub(super) fn begin(odb: &'a mut NativeOdb) -> Res<Self> {
        let begun = odb.begin_batch()?;
        Ok(Batch {
            odb,
            begun,
            wrote: false,
            kept: false,
            ended: false,
        })
    }

    /// Writes those of the pack's objects that are among `added`.
    pub(super) fn write(
        &mut self,
        pack: Option<&PreparedPack>,
        added: &KeyedIdSet<ObjectId>,
    ) -> Res<()> {
        let Some((ip, order)) = pack else {
            return Ok(());
        };
        for &(offset, i) in order {
            let oid = ip.idx().oid_at(i);
            if added.contains(&oid) {
                self.wrote = true;
                let obj = ip.read_at(offset)?;
                self.odb.put(oid, obj.kind, &obj.data)?;
            }
        }
        Ok(())
    }

    /// The objects stay from here on, however the batch ends.
    pub(super) fn keep(&mut self) {
        self.kept = true;
    }

    /// Ends a batch that is kept, making its objects durable.
    pub(super) fn end(mut self) -> Result<(), alt_odb::OdbError> {
        self.ended = true;
        self.odb.flush()
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        if self.wrote
            && !self.kept
            && let Err(e) = self.odb.rewind(self.begun)
        {
            eprintln!("objects of a push that failed left in place: {e}");
        }
        if let Err(e) = self.odb.flush_pending() {
            eprintln!("ending the batch of a push that failed: {e}");
        }
    }
}
