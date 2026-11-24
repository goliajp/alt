//! A walk below a base ends once the pack is waited for no longer.

use alt_git_pack::PackError;
use alt_git_pack::walk::{Budget, Deltas, Objects, Of, Walk};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// A plain entry and deltas by offset, each of the entry before it.
struct Chain {
    payloads: Vec<Vec<u8>>,
    made: Vec<u32>,
    given_up: Arc<AtomicBool>,
    /// how many deltas are made before the pack is given up
    patience: usize,
}

impl Objects for Chain {
    type Id = u32;
    type Error = PackError;

    fn payload<'a>(&'a mut self, entry: u32, _: &'a mut Vec<u8>) -> Result<&'a [u8], PackError> {
        Ok(&self.payloads[entry as usize])
    }

    fn reached(&self, _: u32) -> bool {
        false
    }

    fn made(&mut self, delta: u32, _: u32, _: u32, _: &[u8]) -> Result<u32, PackError> {
        self.made.push(delta);
        if self.made.len() == self.patience {
            self.given_up.store(true, Relaxed);
        }
        Ok(delta)
    }
}

fn walked(patience: usize) -> (Result<(), PackError>, Vec<u32>) {
    // four bytes made of four bytes, all of them new
    let script = vec![4, 4, 4, b'b', b'b', b'b', b'b'];
    let given_up = Arc::new(AtomicBool::new(false));
    let chain = Chain {
        payloads: vec![b"aaaa".to_vec(), script.clone(), script.clone(), script],
        made: Vec::new(),
        given_up: given_up.clone(),
        patience,
    };
    let deltas = Deltas::new([Of::Nothing, Of::Entry(0), Of::Entry(1), Of::Entry(2)]);
    let mut budget = Budget::new((1 << 20, 1 << 20), (1 << 20, 0), 100, 0);
    budget.wait.gone = Some(&given_up);
    let mut walk = Walk::new(chain, &deltas, &budget);
    let walked = walk.below(0, &0, 0);
    (walked, walk.objects.made)
}

#[test]
fn a_walk_ends_at_the_next_delta_once_the_pack_is_waited_for_no_longer() {
    let (whole, made) = walked(usize::MAX);
    assert!(whole.is_ok(), "{whole:?}");
    assert_eq!(made, [1, 2, 3]);
    let (ended, made) = walked(1);
    assert!(matches!(ended, Err(PackError::GivenUp)), "{ended:?}");
    assert_eq!(made, [1]);
}
