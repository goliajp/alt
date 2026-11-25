use super::*;

const LIMITS: Limits = Limits {
    grace: Duration::from_secs(20),
    window: Duration::from_secs(30),
    floor: 30 << 10,
    linger: Duration::from_secs(2),
    each: 64,
    spool: 4 << 30,
};

#[test]
fn a_body_goes_on_while_it_moves_and_is_over_in_the_window_in_which_it_does_not() {
    let mut pace = Pace::new(false);
    assert!(pace.wait(&LIMITS).unwrap() <= LIMITS.window);
    // a window that ends with enough moved in it begins another, and
    // that for as long as the peer keeps it up: a day of windows here
    for _ in 0..2880 {
        pace.moved = LIMITS.floor;
        pace.since = Instant::now() - LIMITS.window;
        let left = pace.wait(&LIMITS).unwrap();
        assert!(left > Duration::from_secs(29) && pace.moved == 0);
    }
    // within a window nothing is asked of what moved
    pace.since = Instant::now() - Duration::from_secs(29);
    assert!(pace.wait(&LIMITS).unwrap() <= Duration::from_secs(1));
    // one byte short at the end of one is the end
    pace.moved = LIMITS.floor - 1;
    pace.since = Instant::now() - LIMITS.window;
    assert_eq!(
        pace.wait(&LIMITS).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn what_comes_of_a_head_buys_it_no_time() {
    let mut pace = Pace::new(true);
    pace.moved = 1 << 30;
    pace.since = Instant::now() - Duration::from_secs(19);
    assert!(pace.wait(&LIMITS).unwrap() <= Duration::from_secs(1));
    pace.since = Instant::now() - Duration::from_secs(21);
    assert!(pace.wait(&LIMITS).is_err());
}

extern "C" fn handled(_: i32) {}

unsafe extern "C" {
    fn pthread_self() -> usize;
    fn pthread_kill(thread: usize, signal: i32) -> i32;
}

/// A read that has a time to wait is broken into by a signal, and the system
/// does not begin it again: it is gone on with, within the time the peer has,
/// and gives what the peer then sends.
#[test]
fn a_read_a_signal_breaks_into_is_gone_on_with() {
    const SIGUSR1: i32 = if cfg!(target_os = "linux") { 10 } else { 30 };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let mut source = Source::new(listener.accept().unwrap().0, LIMITS, None);
    let handler = handled as extern "C" fn(i32) as usize;
    unsafe { alt_os::sys::signal(SIGUSR1, handler) };
    let reader = unsafe { pthread_self() };
    let sender = std::thread::spawn(move || {
        // while the read waits, and then what it waits for
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(10));
            assert_eq!(unsafe { pthread_kill(reader, SIGUSR1) }, 0);
        }
        peer.write_all(b"x").unwrap();
        peer
    });
    assert_eq!(source.more().unwrap(), 1);
    drop(sender.join().unwrap());
}
