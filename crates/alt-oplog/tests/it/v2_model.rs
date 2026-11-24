//! What a version 2 log holds, told without the note beside it: the file
//! read from its first frame, and what whoever drives the handles knows
//! of each op. The handles are held to it: none of them takes for looked
//! at or for synced what this does not.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use alt_oplog::v2::block::FileHeader;
use alt_oplog::v2::frame::{KIND_OP, KIND_RECEIPT};
use alt_oplog::v2::receipt::Receipt;
use alt_oplog::v2::scan::Walk;
use alt_oplog::v2::state::State;
use alt_oplog::{Op, OpId};

/// Numbers that follow from a seed.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^ (mixed >> 31)
    }

    /// A number below `bound`, which is not zero.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    pub fn one_in(&mut self, of: u64) -> bool {
        self.below(of) == 0
    }
}

/// An op frame as it lies in the file.
#[derive(Debug, Clone)]
pub struct Laid {
    pub at: u64,
    pub end: u64,
    pub id: OpId,
    pub payload: Vec<u8>,
    pub linked: bool,
    pub index: bool,
    pub workspace: [u8; 8],
    pub retired: bool,
}

/// The log as its file alone tells it: the ops of the frames that follow
/// one another from the first, each held to all a full check asks.
pub struct Reference {
    pub ops: Vec<Laid>,
    /// where the last of those frames ends
    pub end: u64,
    /// the furthest a frame says the log was synced
    pub said_synced: u64,
    /// the generation the log is of, and where the last frame ends that
    /// says the log was cut before it
    pub cut: (u32, u64),
    /// its receipts: where each ends, and the request it is of
    pub receipts: Vec<(u64, [u8; 16])>,
}

pub fn reference(dir: &Path) -> Reference {
    let data = fs::read(dir.join("log")).unwrap();
    let walk = Walk {
        layout: FileHeader::decode(&data).unwrap().layout(),
        data: &data,
        base: 0,
        strict: true,
    };
    let (mut state, mut ops, mut cut_end) = (State::empty(), Vec::new(), 0);
    let mut receipts = Vec::new();
    let walked = walk.run(&mut state, &mut |at, end, view| {
        if view.ext.cut.is_some() {
            cut_end = end;
        }
        if view.head.kind == KIND_RECEIPT {
            let receipt = Receipt::decode(view.body).expect("a receipt reads");
            receipts.push((end, receipt.request));
        }
        if view.head.kind == KIND_OP {
            ops.push(Laid {
                at,
                end,
                id: view.op.unwrap(),
                payload: Op::from_body(view.body)?.payload,
                linked: view.ext.linked.is_some(),
                index: view.ext.index_after.is_some(),
                workspace: view.head.workspace,
                retired: view.ext.retired,
            });
        }
        Ok(())
    });
    walked.expect("the frames of the file hold to the full check");
    Reference {
        ops,
        end: state.end,
        said_synced: state.synced,
        cut: (state.generation, cut_end),
        receipts,
    }
}

impl Reference {
    /// The ops whose frames start at or after `from` and end by `to` that
    /// name something which has to be on disk before the log is synced up
    /// to `to`: the ref transactions, and of each workspace there is then
    /// the op that last changed its index.
    pub fn naming(&self, from: u64, to: u64) -> Vec<&Laid> {
        let mut last = BTreeMap::new();
        let mut named = Vec::new();
        for op in self.ops.iter().filter(|op| op.end <= to) {
            if op.linked && op.at >= from {
                named.push(op);
            }
            match (op.workspace, op.retired, op.index) {
                ([0, 0, 0, 0, 0, 0, 0, 0], ..) => {}
                (workspace, true, _) => drop(last.remove(&workspace)),
                (workspace, false, true) => drop(last.insert(workspace, op)),
                _ => {}
            }
        }
        let staged = last.into_values().filter(|op| op.at >= from && !op.linked);
        named.extend(staged);
        named
    }
}

/// What whoever drives the handles knows of the ops beside the file.
#[derive(Default)]
pub struct Model {
    pub boot: u8,
    /// the boot each op was appended in, by its payload
    pub written: HashMap<Vec<u8>, u8>,
    /// the ops somebody was asked about in the running boot and vouched for
    pub vouched: HashSet<Vec<u8>>,
    /// the ops given, in the running boot, to whoever syncs what ops
    /// name, who did
    pub seen_to: HashSet<Vec<u8>>,
    /// up to where the log is synced, what its ops name with it, as far
    /// as the running boot can know
    pub synced: u64,
    /// up to where the file was fsynced at least: no power cut takes
    /// what lies before
    pub fsynced: u64,
}

impl Model {
    /// The ops of the log that another boot left past where it is synced
    /// and that nobody has vouched for since.
    pub fn unsettled<'a>(&self, log: &'a Reference) -> Vec<&'a Laid> {
        let left = |op: &&Laid| {
            op.at >= self.synced
                && self.written[&op.payload] != self.boot
                && !self.vouched.contains(&op.payload)
        };
        log.ops.iter().filter(left).collect()
    }

    /// Takes the log for synced up to `claimed`, as the note beside it
    /// says it is: that holds only where every op before it that names
    /// something was given to whoever syncs what ops name.
    pub fn synced_to(&mut self, log: &Reference, claimed: u64) {
        if claimed <= self.synced {
            return;
        }
        assert!(claimed <= log.end, "synced to {claimed}, past the log");
        for op in log.naming(self.synced, claimed) {
            let name = String::from_utf8_lossy(&op.payload);
            assert!(
                self.seen_to.contains(&op.payload),
                "the log is recorded as synced up to {claimed}, over {name} at {}, \
                 which nobody was given to see to",
                op.at
            );
        }
        self.synced = claimed;
        self.fsynced = self.fsynced.max(claimed);
    }

    /// The machine went down and came up: what its last boot knew beside
    /// the file is gone, and the log is synced as far as its frames say.
    pub fn rebooted(&mut self, log: &Reference) {
        assert!(log.said_synced <= self.synced);
        self.boot += 1;
        self.vouched.clear();
        self.seen_to.clear();
        self.synced = log.said_synced;
    }
}
