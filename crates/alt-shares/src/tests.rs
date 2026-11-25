use super::*;
use std::thread::JoinHandle;

fn at(from: u8) -> Option<IpAddr> {
    Some(IpAddr::from([192, 0, 2, from]))
}

fn taker(shares: &Arc<Shares>, from: u8, bytes: u64) -> Result<Share, Full> {
    let mut share = shares.share(at(from));
    share.take(bytes)?;
    Ok(share)
}

/// a taker of `from` that waits for its turn while `gone` is not set
fn waiter(
    shares: &Arc<Shares>,
    from: u8,
    bytes: u64,
    gone: &Arc<AtomicBool>,
) -> JoinHandle<Result<Share, Full>> {
    let (shares, gone) = (Arc::clone(shares), Arc::clone(gone));
    std::thread::spawn(move || {
        let mut share = shares.share(at(from)).waiting(None, Some(gone));
        share.take(bytes)?;
        Ok(share)
    })
}

fn until_waiting(shares: &Shares, count: usize) {
    let by = Instant::now() + Duration::from_secs(20);
    while shares.waiting() != count {
        assert!(
            Instant::now() < by,
            "{} wait, not {count}",
            shares.waiting()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// the one of `waiters` that is let in next, and what it took
fn next_in(
    waiters: &mut Vec<(&'static str, JoinHandle<Result<Share, Full>>)>,
) -> (&'static str, Share) {
    let by = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(at) = waiters.iter().position(|(_, waiter)| waiter.is_finished()) {
            let (name, waiter) = waiters.remove(at);
            return (name, waiter.join().unwrap().unwrap());
        }
        assert!(Instant::now() < by, "nobody is let in");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_takers_of_one_address_have_a_quarter_between_them_and_the_rest_is_for_others() {
    let shares = Shares::of(1000, 1);
    // one address fills its quarter with many takers, and has no more
    let many: Vec<Share> = (0..5).map(|_| taker(&shares, 1, 50).unwrap()).collect();
    assert!(taker(&shares, 1, 1).is_err());
    assert_eq!(shares.held(), 250);
    // a taker that had some is refused more, and keeps what it had
    let mut one = shares.share(at(1));
    assert!(one.take(1).is_err());
    assert_eq!((one.bytes(), shares.held()), (0, 250));
    // the other addresses have theirs, each its quarter
    let others = [2, 3, 4].map(|from| taker(&shares, from, 250).unwrap());
    assert_eq!(shares.held(), 1000);
    // with all of it taken there is none for a fifth address, nor for this host
    assert!(taker(&shares, 5, 1).is_err());
    assert!(shares.share(None).take(1).is_err());
    drop(others);
    // this host is held to the amount alone
    let mut host = shares.share(None);
    host.take(750).unwrap();
    assert!(host.take(1).is_err());
    drop((many, host));
    assert_eq!(shares.held(), 0);
    assert!(shares.lock().by.is_empty());
}

#[test]
fn an_address_has_one_large_taker_the_server_as_many_as_it_is_told_and_one_alone_whatever_it_needs()
{
    let shares = Shares::of(4000, 2);
    let first = taker(&shares, 1, 1100).unwrap();
    // beside its large taker an address has its quarter for the others
    let small = taker(&shares, 1, 1000).unwrap();
    assert!(taker(&shares, 1, 1).is_err());
    drop(small);
    // and no second large one, though the server may have one more
    assert!(taker(&shares, 1, 1001).is_err());
    let third = taker(&shares, 3, 1100).unwrap();
    // with as many large ones as there may be, one that does not wait is refused,
    // and is not left waiting
    assert!(taker(&shares, 4, 1001).is_err());
    assert_eq!(shares.waiting(), 0);
    let fourth = taker(&shares, 4, 1000).unwrap();
    drop(first);
    let mut fourth_large = taker(&shares, 4, 1001).unwrap();
    assert_eq!(shares.held(), 1100 + 1000 + 1001);
    drop((third, fourth));
    // a large one has half at the most
    fourth_large.take(999).unwrap();
    assert!(fourth_large.take(1).is_err());
    drop(fourth_large);
    assert_eq!(shares.held(), 0);
    assert!(shares.lock().large.is_empty() && shares.lock().by.is_empty());
    // one past half is refused at once
    assert!(taker(&shares, 5, 2001).is_err());
    // unless the amount is one that a taker alone may pass
    let shares = Shares::passed_by_one_alone(1000, 1);
    let mut only = taker(&shares, 1, 5000).unwrap();
    assert!(taker(&shares, 2, 1).is_err());
    only.take(900).unwrap();
    drop(only);
    let other = taker(&shares, 2, 1).unwrap();
    assert!(taker(&shares, 1, 1000).is_err());
    assert!(taker(&shares, 1, 501).is_err());
    let large = taker(&shares, 1, 500).unwrap();
    drop((other, large));
    assert_eq!(shares.held(), 0);
}

#[test]
fn takers_that_wait_to_be_large_are_let_in_an_address_at_a_time_in_turn() {
    let shares = Shares::of(10_000, 1);
    let gone = Arc::new(AtomicBool::new(false));
    let first = taker(&shares, 1, 3000).unwrap();
    assert!(!first.waited_for());
    let mut waiters = Vec::new();
    // one more of the first address, two of a second, one of a third
    for (count, (name, from)) in [("a2", 1), ("b1", 2), ("b2", 2), ("c1", 3)]
        .into_iter()
        .enumerate()
    {
        waiters.push((name, waiter(&shares, from, 3000, &gone)));
        until_waiting(&shares, count + 1);
    }
    assert!(first.waited_for());
    let mut order = Vec::new();
    drop(first);
    while !waiters.is_empty() {
        let (name, share) = next_in(&mut waiters);
        order.push(name);
        // the one let in is the only one in, until it is gone
        std::thread::sleep(Duration::from_millis(50));
        assert!(waiters.iter().all(|(_, waiter)| !waiter.is_finished()));
        assert_eq!(shares.held(), 3000);
        drop(share);
    }
    // the second of the second address does not keep the third waiting
    assert_eq!(order, ["a2", "b1", "c1", "b2"]);
    assert_eq!((shares.held(), shares.waiting()), (0, 0));
}

#[test]
fn a_taker_waits_while_it_is_waited_for_and_one_let_in_in_vain_gives_its_turn_on() {
    let shares = Shares::of(1000, 2);
    let gone = Arc::new(AtomicBool::new(false));
    let first = taker(&shares, 1, 300).unwrap();
    // another address does not wait while there is a turn free
    let other = waiter(&shares, 2, 300, &gone).join().unwrap().unwrap();
    assert!(!first.waited_for());
    // one of the same address does
    let second = waiter(&shares, 1, 300, &gone);
    until_waiting(&shares, 1);
    assert!(first.waited_for() && !other.waited_for());
    gone.store(true, Ordering::Relaxed);
    assert!(second.join().unwrap().is_err());
    assert_eq!(shares.waiting(), 0);
    drop((first, other));
    // one told a time waits until then
    let first = taker(&shares, 1, 300).unwrap();
    let began = Instant::now();
    let by = Some(began + Duration::from_millis(300));
    assert!(shares.share(at(1)).waiting(by, None).take(300).is_err());
    assert!(began.elapsed() >= Duration::from_millis(300));
    drop(first);

    // let in where it cannot be large after all: past half, and not alone
    let shares = Shares::passed_by_one_alone(1000, 1);
    let gone = Arc::new(AtomicBool::new(false));
    let first = taker(&shares, 1, 300).unwrap();
    let past_half = waiter(&shares, 2, 600, &gone);
    until_waiting(&shares, 1);
    let small = taker(&shares, 3, 100).unwrap();
    drop(first);
    assert!(past_half.join().unwrap().is_err());
    // its turn is given on
    let next = taker(&shares, 4, 300).unwrap();
    drop((small, next));
    assert_eq!(shares.held(), 0);
    assert!(shares.lock().large.is_empty() && shares.lock().let_in.is_empty());
}

#[test]
fn a_large_taker_another_waits_for_is_slow_below_its_pace_and_one_alone_is_not() {
    let shares = Shares::of(1000, 1).paced(100, Duration::from_secs(10));
    let gone = Arc::new(AtomicBool::new(false));
    let mut large = taker(&shares, 1, 300).unwrap();
    let mut small = taker(&shares, 2, 100).unwrap();
    let began = Instant::now();
    let at = |seconds: u64| began + Duration::from_secs(seconds);
    // alone in its turn it is as slow as it is
    large.sent_at(1, began).unwrap();
    large.sent_at(1, at(20)).unwrap();
    let waiting = waiter(&shares, 3, 300, &gone);
    until_waiting(&shares, 1);
    // a small one is never slow; a large one fast enough is not
    small.sent_at(1, began).unwrap();
    small.sent_at(1, at(20)).unwrap();
    large.sent_at(999, at(29)).unwrap();
    large.sent_at(1, at(30)).unwrap();
    // in the next window it takes a byte too few
    large.sent_at(998, at(39)).unwrap();
    assert_eq!(large.sent_at(1, at(40)), Err(Slow));
    drop(large);
    let waiting = waiting.join().unwrap().unwrap();
    drop((waiting, small));
    assert_eq!(shares.held(), 0);
}

#[test]
fn a_peer_is_counted_under_its_address_or_the_network_of_its_host() {
    let under = |peer: &str| address(peer.parse().unwrap()).map(|under| under.to_string());
    for here in ["127.0.0.1", "::1", "::ffff:127.0.0.9"] {
        assert_eq!(under(here), None, "{here}");
    }
    assert_eq!(under("203.0.113.7").unwrap(), "203.0.113.7");
    assert_eq!(under("::ffff:203.0.113.7").unwrap(), "203.0.113.7");
    assert_eq!(under("2001:db8:1:2:3:4:5:6").unwrap(), "2001:db8:1:2::");
    assert_eq!(under("fe80::1:2").unwrap(), "fe80::1:2");
    let free = half_of_what_is_free(&std::env::temp_dir()).unwrap();
    assert!(free > 0);
}
