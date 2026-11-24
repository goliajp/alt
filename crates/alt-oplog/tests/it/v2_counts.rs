//! How often the sequences driven on version 2 logs did each kind of
//! thing they are drawn to do (`v2_sequences`).

use std::sync::atomic::AtomicU64;

/// How often a run on blocks of one size did each kind of thing.
pub struct Counts {
    pub sequences: AtomicU64,
    pub cuts: AtomicU64,
    pub cuts_died: AtomicU64,
    pub cut_back: AtomicU64,
    /// of them, those whose ops the log held: a cut of them was begun
    pub cut_back_begun: AtomicU64,
    /// of them, those told by their sinks, of what the sinks were left
    pub told_by_sink: AtomicU64,
    pub meanwhile: AtomicU64,
    pub meanwhile_cuts: AtomicU64,
    pub receipts: AtomicU64,
    pub anchored: AtomicU64,
    pub anchored_gone: AtomicU64,
    pub grown_back: AtomicU64,
    pub grown_back_after_death: AtomicU64,
    pub hidden: AtomicU64,
    /// sequences with the note beside the log kept, meddled with, and
    /// never written
    pub kept: AtomicU64,
    pub meddled: AtomicU64,
    pub never: AtomicU64,
    pub notes_gone: AtomicU64,
    pub notes_spoiled: AtomicU64,
    /// handles that came upon a cut that died after each of its steps
    /// with ops held past it
    pub came_upon: [AtomicU64; 5],
}

impl Counts {
    const fn new() -> Self {
        Counts {
            sequences: AtomicU64::new(0),
            cuts: AtomicU64::new(0),
            cuts_died: AtomicU64::new(0),
            cut_back: AtomicU64::new(0),
            cut_back_begun: AtomicU64::new(0),
            told_by_sink: AtomicU64::new(0),
            meanwhile: AtomicU64::new(0),
            meanwhile_cuts: AtomicU64::new(0),
            receipts: AtomicU64::new(0),
            anchored: AtomicU64::new(0),
            anchored_gone: AtomicU64::new(0),
            grown_back: AtomicU64::new(0),
            grown_back_after_death: AtomicU64::new(0),
            hidden: AtomicU64::new(0),
            kept: AtomicU64::new(0),
            meddled: AtomicU64::new(0),
            never: AtomicU64::new(0),
            notes_gone: AtomicU64::new(0),
            notes_spoiled: AtomicU64::new(0),
            came_upon: [const { AtomicU64::new(0) }; 5],
        }
    }
}

/// For each of three batches of seeds, for blocks of 1, 4 and 16 KiB.
pub static COUNTS: [[Counts; 3]; 3] = [const { [const { Counts::new() }; 3] }; 3];

impl Counts {
    /// Each count with what it is a count of.
    pub fn named(&self) -> Vec<(&'static str, &AtomicU64)> {
        let came = |step: usize| &self.came_upon[step - 1];
        let told_begun = "of them told so of ops the log held, a cut of them begun";
        let told_by_sink = "of them told by their sinks, of what the sinks were left";
        vec![
            ("sequences", &self.sequences),
            ("with the note beside the log kept", &self.kept),
            ("with it meddled with", &self.meddled),
            ("with none ever written", &self.never),
            ("notes removed before a read", &self.notes_gone),
            ("notes spoiled before a read", &self.notes_spoiled),
            ("cuts of ops nobody vouched for", &self.cuts),
            ("handles told their ops were cut", &self.cut_back),
            (told_begun, &self.cut_back_begun),
            (told_by_sink, &self.told_by_sink),
            ("handles past a cut that died after step 1", came(1)),
            ("after step 2", came(2)),
            ("after step 3", came(3)),
            ("after step 4", came(4)),
            ("after step 5", came(5)),
            (
                "things done while a writer had given up the lock",
                &self.meanwhile,
            ),
            ("of them looks that cut the log", &self.meanwhile_cuts),
            ("receipts", &self.receipts),
            ("opens at an anchor that holds", &self.anchored),
            (
                "opens at the anchor of an op that is gone",
                &self.anchored_gone,
            ),
            ("cuts that died", &self.cuts_died),
            ("logs grown back to the length they had", &self.grown_back),
            (
                "of them after a cut that died",
                &self.grown_back_after_death,
            ),
            (
                "reads that ended where the log was noted to end",
                &self.hidden,
            ),
        ]
    }
}
