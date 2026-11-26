//! What a taker gives back while it waits for its turn.

use super::*;
use std::sync::atomic::AtomicUsize;

/// gives back nothing; counts how often it was asked to, and takes back as told
struct Counted(Arc<(AtomicUsize, AtomicUsize)>, bool);

impl Aside for Counted {
    fn step_aside(&mut self) {
        self.0.0.fetch_add(1, Ordering::SeqCst);
    }
    fn step_back(&mut self) -> bool {
        self.0.1.fetch_add(1, Ordering::SeqCst);
        self.1
    }
}

#[test]
fn a_taker_steps_aside_while_it_waits_and_back_once_it_is_let_in() {
    let shares = Shares::of(1000, 1);
    let first = taker(&shares, 1, 300).unwrap();
    let waiting = |back: bool| {
        let counts = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let (shares, told) = (Arc::clone(&shares), Arc::clone(&counts));
        let waiter = std::thread::spawn(move || {
            let aside = Box::new(Counted(told, back));
            let mut share = shares.share(at(2)).waiting(None, None).aside(aside);
            share.take(300).map(|()| share)
        });
        (counts, waiter)
    };
    let (counts, waiter) = waiting(true);
    until_waiting(&shares, 1);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(counts.0.load(Ordering::SeqCst), 1);
    assert_eq!(counts.1.load(Ordering::SeqCst), 0);
    drop(first);
    let second = waiter.join().unwrap().unwrap();
    assert_eq!(counts.1.load(Ordering::SeqCst), 1);
    // one that is not had back gives its turn on
    let (counts, waiter) = waiting(false);
    until_waiting(&shares, 1);
    drop(second);
    assert!(waiter.join().unwrap().is_err());
    assert_eq!(counts.1.load(Ordering::SeqCst), 1);
    assert!(taker(&shares, 3, 300).is_ok());
    // a taker let in at once steps nowhere
    let (counts, waiter) = waiting(true);
    drop(waiter.join().unwrap().unwrap());
    assert_eq!(counts.0.load(Ordering::SeqCst), 0);
}

/// counts how often the taker is told it has its turn after it was put off
struct Turned(Arc<AtomicUsize>);

impl Aside for Turned {
    fn step_aside(&mut self) {}
    fn step_back(&mut self) -> bool {
        true
    }
    fn turned(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_taker_put_off_is_told_once_it_has_its_turn() {
    let shares = Shares::of(1000, 1);
    let first = taker(&shares, 1, 300).unwrap();
    let told = Arc::new(AtomicUsize::new(0));
    let aside = Box::new(Turned(Arc::clone(&told)));
    let mut share = (shares.share(at(2)).waiting(None, None))
        .aside(aside)
        .turn_first();
    assert!(share.take(300).is_err() && share.put_off());
    assert_eq!(told.load(Ordering::SeqCst), 0);
    let waiter = std::thread::spawn(move || share.take_turn().map(|()| share));
    until_waiting(&shares, 1);
    drop(first);
    let mut share = waiter.join().unwrap().unwrap();
    assert_eq!(told.load(Ordering::SeqCst), 1);
    // being large, it takes the rest with nothing more to wait for
    share.take(300).unwrap();
    assert_eq!(told.load(Ordering::SeqCst), 1);
}
