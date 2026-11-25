use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::time::Duration;

#[derive(Default)]
pub(crate) struct Held {
    pub(crate) all: u64,
    /// what the takers of each address that keep within the part have between them
    pub(crate) by: HashMap<IpAddr, u64>,
    /// the addresses that have a large taker, or one let in to be one
    pub(crate) large: HashSet<IpAddr>,
    /// the takers that wait to be large, by address, the oldest first
    pub(crate) waiting: HashMap<IpAddr, VecDeque<u64>>,
    /// the addresses that have takers waiting, in the order their turns come
    pub(crate) turns: VecDeque<IpAddr>,
    /// takers let in to be large that have not yet seen it
    pub(crate) let_in: HashSet<u64>,
    /// the number the next taker that waits is known by
    pub(crate) next: u64,
    /// what a large taker another waits for sends at the least, and over what time
    pub(crate) pace: (u64, Duration),
}

impl Held {
    /// Lets in as many of the takers that wait as there is room for large
    /// ones, the oldest of an address at a time, the addresses in turn.
    pub(crate) fn let_in(&mut self, most: usize) {
        let mut passed = 0;
        while self.large.len() < most && passed < self.turns.len() {
            let from = self.turns.pop_front().expect("a turn");
            if self.large.contains(&from) {
                self.turns.push_back(from);
                passed += 1;
                continue;
            }
            let queue = self.waiting.get_mut(&from).expect("takers that wait");
            let taker = queue.pop_front().expect("a taker");
            if queue.is_empty() {
                self.waiting.remove(&from);
            } else {
                self.turns.push_back(from);
            }
            self.large.insert(from);
            self.let_in.insert(taker);
            passed = 0;
        }
    }

    /// the taker `taker` of `from` waits no more, and was not let in
    pub(crate) fn waits_no_more(&mut self, from: IpAddr, taker: u64) {
        let queue = self.waiting.get_mut(&from).expect("takers that wait");
        queue.retain(|waiting| *waiting != taker);
        if queue.is_empty() {
            self.waiting.remove(&from);
            self.turns.retain(|turn| *turn != from);
        }
    }
}
