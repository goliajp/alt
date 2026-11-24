//! What goes wrong around a driven version 2 log: a writer that dies in
//! the middle of an append, and the machine going down.

use std::fs;
use std::io::Write;

use alt_oplog::OpLog;

use super::v2_cut_told::tell_cut;
use super::v2_driver::Run;
use super::v2_log::boot;

impl Run {
    /// A writer dies in the middle of an append: it has read the log to
    /// its end under the lock, as every writer does, and leaves there any
    /// bytes, or the first bytes of a frame.
    pub fn tear(&mut self) {
        let len = 1 + self.rng.below(120);
        self.tear_of(len);
    }

    /// [`Self::tear`], with `len` bytes left where there is no frame to
    /// leave the first bytes of.
    pub fn tear_of(&mut self, len: u64) {
        let mut writer = OpLog::open(self.path()).expect("the log opens");
        writer.refresh().expect("a writer reads the log to its end");
        let told = writer.take_discarded();
        drop(writer);
        self.cuts_told(&told, None);
        self.ledger.begun = None;
        let path = self.path().join("log");
        let whole = fs::read(&path).unwrap();
        let mut torn: Vec<u8> = (0..len).map(|_| self.rng.next() as u8).collect();
        let short = len <= 120 && self.rng.one_in(2);
        if let Some(last) = self.ledger.ops.last().filter(|_| short) {
            let frame = &whole[last.at as usize..last.end as usize];
            torn = frame[..1 + self.rng.below(frame.len() as u64 - 1) as usize].to_vec();
        }
        self.trace.push(format!(
            "{} bytes of no frame at {}",
            torn.len(),
            whole.len()
        ));
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&torn).unwrap();
    }

    /// The machine goes down: the handles are gone, and of what lies past
    /// where the file was synced a part may be. The ops whose frames end
    /// before the first byte that changed are in the log after, and no
    /// other. With `whole`, nothing is lost.
    pub fn reboot(&mut self, whole: bool) {
        self.handles.clear();
        let path = self.path().join("log");
        let before = fs::read(&path).unwrap();
        let len = before.len() as u64;
        let from = self.model.fsynced.max(64);
        let lost = match self.rng.below(4) {
            _ if whole => "nothing is lost".to_owned(),
            0 if len > from => {
                let kept = from + self.rng.below(len - from + 1);
                let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.set_len(kept).unwrap();
                format!("the file ends at {kept}")
            }
            1 if len > from => {
                let start = from + self.rng.below(len - from);
                let end = start + 1 + self.rng.below(len - start);
                let mut bytes = before.clone();
                bytes[start as usize..end as usize].fill(0);
                fs::write(&path, bytes).unwrap();
                format!("{start}..{end} is zeros")
            }
            _ => "nothing is lost".to_owned(),
        };
        let after = fs::read(&path).unwrap();
        let same = before.iter().zip(&after).take_while(|(was, is)| was == is);
        let same = same.count() as u64;
        if same < len {
            self.ledger.lost_from(same, self.model.synced);
        }
        self.ledger.hides.clear();
        // what the header of the log tells of a cut that never came to
        // it was not synced: it is there after, or it is not
        if self.ledger.begun.is_some() && !whole && self.also.one_in(2) {
            tell_cut(self.path(), (0, 0));
            self.ledger.begun = None;
        }
        let log = self.log();
        self.model.rebooted(&log);
        boot(self.model.boot);
        self.trace
            .push(format!("boot {} of {len} bytes: {lost}", self.model.boot));
    }
}
