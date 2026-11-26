//! A taker that has its turn before it is built: put off where it would wait,
//! holding nothing while it waits.

use super::*;

#[test]
fn a_taker_that_has_its_turn_first_is_put_off_where_it_would_wait_and_waits_holding_nothing() {
    let shares = Shares::of(1000, 1);
    let first = taker(&shares, 1, 300).unwrap();

    // put off where it takes past its part
    let mut put = shares.share(at(2)).waiting(None, None).turn_first();
    put.take(100).unwrap();
    assert!(put.take(200).is_err());
    assert!(put.put_off());
    assert_eq!((shares.held(), shares.waiting()), (400, 0));
    // and where it is to: told what it is to take
    let mut told = shares.share(at(3)).waiting(None, None).turn_first();
    assert!(told.expect(200).is_ok());
    assert!(told.expect(300).is_err() && told.put_off());
    assert_eq!((shares.held(), shares.waiting()), (400, 0));
    drop(told);

    // it gives back what it took while it waits, and has the turn after the first
    let waiter = std::thread::spawn(move || put.take_turn().map(|()| put));
    until_waiting(&shares, 1);
    assert_eq!(shares.held(), 300);
    drop(first);
    let mut put = waiter.join().unwrap().unwrap();
    assert!(!put.put_off());
    put.take(400).unwrap();
    assert_eq!(put.bytes(), 400);
    // the turn is its own: another large one waits, and is put off
    let mut after = shares.share(at(4)).waiting(None, None).turn_first();
    assert!(after.expect(300).is_err() && after.put_off());
    drop(put);
    assert!(after.expect(300).is_ok());
    after.take(300).unwrap();
    drop(after);
    assert_eq!(shares.held(), 0);

    // one where the turn is free, and one of this host, are not put off
    let mut free = shares.share(at(5)).turn_first();
    free.take(400).unwrap();
    let mut here = shares.share(None).turn_first();
    assert!(here.expect(400).is_ok());
    here.take(400).unwrap();
    assert!(!free.put_off() && !here.put_off());
}
