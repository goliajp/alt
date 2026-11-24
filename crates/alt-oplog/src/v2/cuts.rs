//! The generation a frame from before a cut was written under.
//!
//! A frame reads under the generation the log was of when it was written.
//! The checkpoint a cut leaves says under which one the frames before it
//! were written, back to the checkpoint of the cut before, which it names.
//! A handle that looks a frame up before the last cut reads those
//! checkpoints, the header and what it carries beside its body of each,
//! from the last cut back to the first one that does not lie after the
//! frame, and keeps what it read: as many small reads as cuts were made
//! since the earliest frame it looked up, however many the log had before
//! and however many frames it looks up after.

use crate::OpLogError;
use crate::v2::block::BLOCK_HEADER_LEN;
use crate::v2::frame::{Cut, Ext, HEADER_LEN, KIND_CHECKPOINT};
use crate::v2::log::Log;
use crate::v2::mark::Mark;

/// The cuts of a log whose last one left the checkpoint at `last` under
/// `generation`, as far back as they were read: where the checkpoint of
/// each starts and the generation of the frames before it, from the last
/// one back.
#[derive(Default)]
pub(crate) struct Cuts {
    last: u64,
    generation: u32,
    back: Vec<(u64, u32)>,
    /// where the checkpoint of the cut before those starts, which is not
    /// read yet: zero where none is left or they do not lead further back
    next: u64,
}

/// more than the checkpoint of a cut carries beside its body
const CARRIED: usize = 256;

impl Log {
    /// What the checkpoint frame at `at` says of the cut it was left by,
    /// and of the one before: where the checkpoint of that one starts
    /// and the generation of the frames between the two. Its checksum
    /// is not held to.
    fn cut_said(&self, at: u64) -> std::io::Result<Option<Cut>> {
        let Some(left) = self.state.end.checked_sub(at) else {
            return Ok(None);
        };
        let reach = left.min((HEADER_LEN + CARRIED + BLOCK_HEADER_LEN) as u64);
        let data = self.source().at(at, reach as usize)?;
        let said = |len: usize| self.layout.lift_start(&data, at, at, len);
        let Some(header) = said(HEADER_LEN) else {
            return Ok(None);
        };
        let carried = u16::from_le_bytes([header[2], header[3]]) as usize;
        if header[0] != KIND_CHECKPOINT || carried > CARRIED {
            return Ok(None);
        }
        let ext = said(HEADER_LEN + carried).and_then(|frame| Ext::decode(&frame[HEADER_LEN..]));
        Ok(ext.and_then(|ext| ext.cut))
    }

    /// Whether the header tells of a cut begun where the log ends for
    /// this handle, at what does not read: the checkpoint of a cut of
    /// ops may lie there, lost in part when the power went, and the
    /// writer that reads the log next makes that cut again.
    fn cut_lost_here(&self) -> Result<bool, OpLogError> {
        let told = Mark::read_unlocked(&self.file)?;
        let (end, size) = (self.state.end, self.file.metadata()?.len());
        if !told.begun(self.state.generation) || told.at != end || size <= end {
            return Ok(false);
        }
        Ok(self.read_past(size)?.0.end == end)
    }

    /// Where the checkpoints start, past byte `from`, of the cuts that
    /// took ops off the log, the last cut first. A cut of what a writer
    /// left unfinished took nothing a sync can have ended at, and is
    /// none of them. One whose checkpoint was lost is among them before
    /// it is made again.
    pub(super) fn cuts_past(&self, from: u64) -> Result<Vec<u64>, OpLogError> {
        let (mut at, mut found) = (self.state.cut, Vec::new());
        if self.cut_lost_here()? {
            found.push(self.state.end);
        }
        while at > from {
            let Some(cut) = self.cut_said(at)? else {
                return Err(OpLogError::Format("oplog record corrupt"));
            };
            if cut.of_the_log {
                found.push(at);
            }
            // each lies before the one that names it
            at = if cut.after < at { cut.after } else { 0 };
        }
        Ok(found)
    }

    /// The generation the frame at `at` was written under, when it lies
    /// before the last cut the log tells of: `None` after it, where the
    /// frames are of the generation the log has now, and where the
    /// checkpoints of the cuts do not lead back that far.
    pub(super) fn generation_before_cut(&self, at: u64) -> std::io::Result<Option<u32>> {
        if at >= self.state.cut {
            return Ok(None);
        }
        let mut cuts = self.cuts.borrow_mut();
        if (cuts.last, cuts.generation) != (self.state.cut, self.state.generation) {
            *cuts = Cuts {
                last: self.state.cut,
                generation: self.state.generation,
                back: Vec::new(),
                next: self.state.cut,
            };
        }
        while cuts.next > at {
            let cut = cuts.next;
            cuts.next = match self.cut_said(cut)? {
                Some(said) => {
                    cuts.back.push((cut, said.generation));
                    // each lies before the one that names it
                    if said.after < cut { said.after } else { 0 }
                }
                None => 0,
            };
        }
        // the earliest cut after the frame tells
        let after = cuts.back.iter().take_while(|(cut, _)| *cut > at).last();
        Ok(after.map(|(_, generation)| *generation))
    }
}
