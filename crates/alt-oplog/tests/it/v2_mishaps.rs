//! What goes wrong around a driven version 2 log: a writer that dies in
//! the middle of an append, and the machine going down.

use std::fs;
use std::io::Write;

use alt_oplog::OpLog;

use super::v2_driver::Run;
use super::v2_outage::Loss;

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

    /// The machine goes down, and of what was not synced any part may
    /// be lost; with `whole`, nothing is.
    pub fn reboot(&mut self, whole: bool) {
        self.reboot_as(if whole { Loss::Nothing } else { Loss::Any });
    }
}
