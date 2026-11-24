//! How often the sequences driven on version 2 logs did each kind of
//! thing they are drawn to do (`v2_sequences`).

use std::sync::atomic::AtomicU64;

/// How often a run on blocks of one size did each kind of thing.
pub struct Counts {
    pub sequences: AtomicU64,
    /// of them, those whose fsyncs were made; and the fsyncs asked for
    pub real: AtomicU64,
    pub fsyncs: AtomicU64,
    pub cuts: AtomicU64,
    pub cuts_died: AtomicU64,
    pub cut_back: AtomicU64,
    /// of them, those whose ops the log held: a cut of them was begun
    pub cut_back_begun: AtomicU64,
    /// of them, those that read the log anew with nobody told
    pub untold: AtomicU64,
    /// callers without a ticket told by a handle that frames were cut
    pub own_cut: AtomicU64,
    /// frames left to a sink, for a caller without a ticket, that the
    /// sink synced, and that it told of a cut for
    pub left_synced: AtomicU64,
    pub left_told: AtomicU64,
    /// tickets taken; those answered as synced, as cut, and as owed an
    /// answer by a sink still; and those answered when the sequence was
    /// over
    pub claims: AtomicU64,
    pub claims_synced: AtomicU64,
    pub claims_cut: AtomicU64,
    pub claims_owed: AtomicU64,
    pub claims_late: AtomicU64,
    /// tickets dropped, tickets told of a cut that asked again, and
    /// syncs that promised nothing
    pub claims_dropped: AtomicU64,
    pub claims_again: AtomicU64,
    pub unpromised: AtomicU64,
    /// syncs refused a boundary over an op nobody looked at
    pub unjudged: AtomicU64,
    /// of them, over an op the driver knows nobody has to look at
    pub unjudged_more: AtomicU64,
    /// cuts refused before a frame to keep, and within what is synced
    pub kept_refused: AtomicU64,
    pub lost_refused: AtomicU64,
    /// of the cuts refused before a frame to keep: those where the log
    /// was recorded as synced over a frame past the op that is one to
    /// keep, and those where it never was
    pub kept_needed: AtomicU64,
    pub kept_uncovered: AtomicU64,
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
            real: AtomicU64::new(0),
            fsyncs: AtomicU64::new(0),
            cuts: AtomicU64::new(0),
            cuts_died: AtomicU64::new(0),
            cut_back: AtomicU64::new(0),
            cut_back_begun: AtomicU64::new(0),
            untold: AtomicU64::new(0),
            own_cut: AtomicU64::new(0),
            left_synced: AtomicU64::new(0),
            left_told: AtomicU64::new(0),
            claims: AtomicU64::new(0),
            claims_synced: AtomicU64::new(0),
            claims_cut: AtomicU64::new(0),
            claims_owed: AtomicU64::new(0),
            claims_late: AtomicU64::new(0),
            claims_dropped: AtomicU64::new(0),
            claims_again: AtomicU64::new(0),
            unpromised: AtomicU64::new(0),
            unjudged: AtomicU64::new(0),
            unjudged_more: AtomicU64::new(0),
            kept_refused: AtomicU64::new(0),
            lost_refused: AtomicU64::new(0),
            kept_needed: AtomicU64::new(0),
            kept_uncovered: AtomicU64::new(0),
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

/// For the sequences the environment names.
pub static NAMED: Counts = Counts::new();

impl Counts {
    /// Each count with what it is a count of.
    pub fn named(&self) -> Vec<(&'static str, &AtomicU64)> {
        let came = |step: usize| &self.came_upon[step - 1];
        let told_begun = "of them told so of ops the log held, a cut of them begun";
        let untold = "of them read the log anew with nobody told";
        vec![
            ("sequences", &self.sequences),
            ("of them with their fsyncs made", &self.real),
            ("fsyncs asked for", &self.fsyncs),
            ("with the note beside the log kept", &self.kept),
            ("with it meddled with", &self.meddled),
            ("with none ever written", &self.never),
            ("notes removed before a read", &self.notes_gone),
            ("notes spoiled before a read", &self.notes_spoiled),
            ("cuts of ops nobody vouched for", &self.cuts),
            ("handles told their ops were cut", &self.cut_back),
            (told_begun, &self.cut_back_begun),
            (untold, &self.untold),
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
        .into_iter()
        .chain(self.named_of_syncs())
        .collect()
    }

    /// The counts of what became of syncs and of cuts that were refused.
    fn named_of_syncs(&self) -> Vec<(&'static str, &AtomicU64)> {
        vec![
            (
                "callers without a ticket told of a cut by a handle",
                &self.own_cut,
            ),
            ("frames left to a sink that it synced", &self.left_synced),
            ("that it told of a cut for", &self.left_told),
            ("tickets written on", &self.claims),
            ("dropped", &self.claims_dropped),
            (
                "told of a cut again when they asked again",
                &self.claims_again,
            ),
            ("syncs that promised nothing", &self.unpromised),
            ("answered as synced", &self.claims_synced),
            ("answered as cut", &self.claims_cut),
            ("answered as owed by a sink still", &self.claims_owed),
            ("answered once the sequence was over", &self.claims_late),
            (
                "syncs refused a boundary over an op nobody looked at",
                &self.unjudged,
            ),
            (
                "of them over an op the driver knows needs no look",
                &self.unjudged_more,
            ),
            ("cuts refused before a frame to keep", &self.kept_refused),
            (
                "of them with a frame to keep past the op that a sync covered",
                &self.kept_needed,
            ),
            (
                "and with none that a sync ever covered",
                &self.kept_uncovered,
            ),
            (
                "cuts refused within where the log is synced",
                &self.lost_refused,
            ),
        ]
    }
}
