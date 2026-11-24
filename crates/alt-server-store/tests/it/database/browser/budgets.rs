//! How many sign-ins and device codes a minute are let in.
use super::*;
use alt_server_store::Error;

/// how many attempts the row of `key` has counted in its minute
fn attempts(f: &Fixture, key: &str) -> i32 {
    f.client()
        .query_one(
            "SELECT attempts FROM alt_login_budgets WHERE key=$1",
            &[&key],
        )
        .unwrap()
        .get(0)
}

fn of(kind: &str, network: &str) -> String {
    format!("{kind}:{}", alt_browser_auth::digest(network))
}

fn set(f: &Fixture, key: &str, attempts: i32) {
    let set = "UPDATE alt_login_budgets SET attempts=$2 WHERE key=$1";
    let changed = f.client().execute(set, &[&key, &attempts]).unwrap();
    assert_eq!(changed, 1, "{key}");
}

/// a sign-in attempted from `network`, each for a login nobody else attempts
fn sign_in(f: &mut Fixture, network: &str) -> std::result::Result<(), Error> {
    sign_in_at(&mut f.store, network)
}

fn sign_in_at(store: &mut Store, network: &str) -> std::result::Result<(), Error> {
    let login = format!("u{}", alt_browser_auth::digest(network));
    let network = network.parse().unwrap();
    let admitted = store.admit_browser_password(&origin(), &login[..32], PASSWORD, network);
    admitted.map(|_| ())
}

/// a device code asked for from `network`
fn code(f: &mut Fixture, network: &str) -> std::result::Result<u64, Error> {
    code_at(&mut f.store, network)
}

fn code_at(store: &mut Store, network: &str) -> std::result::Result<u64, Error> {
    let device = alt_browser_auth::digest(&alt_browser_auth::secret());
    let mut random = [0u8; 8];
    alt_os::random::fill(&mut random);
    let human: String = random
        .iter()
        .map(|b| alt_remote_protocol::login::USER_CODE_ALPHABET[(b & 31) as usize] as char)
        .collect();
    let token = alt_browser_auth::token();
    store.begin_device_authorization(
        &origin(),
        &device,
        &human,
        &alt_server_store::cloud_credential_digest(&token).unwrap(),
        network.parse().unwrap(),
    )
}

/// Sign-ins and codes are counted at once, by eight connections, each time just
/// after the minutes of all the rows are over: every one of them then counts its
/// own rows in anew and removes the rows that are over. None waits for a row of
/// another while the other waits for one of its own: all of them are let in.
#[test]
#[ignore = "requires isolated PostgreSQL; transactions that count attempts at once"]
fn attempts_counted_at_once_when_the_minutes_are_over_do_not_end_each_other() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    let mut stores: Vec<Store> = (0..8).map(|_| f.reopen()).collect();
    for round in 0..30 {
        let over = "UPDATE alt_login_budgets SET window_start=window_start-120000";
        f.client().execute(over, &[]).unwrap();
        let together = std::sync::Barrier::new(stores.len());
        let failed: Vec<String> = std::thread::scope(|scope| {
            let counting: Vec<_> = (stores.iter_mut().enumerate())
                .map(|(at, store)| {
                    let together = &together;
                    scope.spawn(move || {
                        let network = format!("198.51.{at}.7");
                        together.wait();
                        match at % 2 {
                            0 => code_at(store, &network).map(|_| ()),
                            _ => sign_in_at(store, &network),
                        }
                    })
                })
                .collect();
            let ended = counting.into_iter().map(|thread| thread.join().unwrap());
            ended
                .filter_map(|ended| ended.err())
                .map(|error| format!("{error:?}"))
                .collect()
        });
        assert!(failed.is_empty(), "round {round}: {failed:?}");
    }
}

#[test]
#[ignore = "requires isolated PostgreSQL; a network past its budget and the budget every network shares"]
fn an_attempt_refused_for_its_network_takes_nothing_from_what_every_network_shares() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    sign_in(&mut f, "192.0.2.1").unwrap();
    set(&f, &of("network", "192.0.2.1"), 100);
    let shared = attempts(&f, "global");
    for _ in 0..3 {
        // refused, and told how long the minute of the address still is
        match sign_in(&mut f, "192.0.2.1") {
            Err(Error::Attempts(left)) => assert!((1..=60_000).contains(&left), "{left}"),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(attempts(&f, "global"), shared);
    sign_in(&mut f, "192.0.2.2").unwrap();
    assert_eq!(attempts(&f, "global"), shared + 1);

    // the addresses of one /64 are one network, those of the next /64 another
    sign_in(&mut f, "2001:db8::1").unwrap();
    set(&f, &of("network", "2001:db8::"), 100);
    assert!(sign_in(&mut f, "2001:db8::2").is_err());
    sign_in(&mut f, "2001:db8:0:1::2").unwrap();
}

/// Past what one address may attempt there is what the network around it may, and
/// past what everybody attempts in a minute an address is held to a few attempts:
/// nobody is refused for the attempts of others alone.
#[test]
#[ignore = "requires isolated PostgreSQL; the budgets of an address, of its network and of everybody"]
fn everybody_being_busy_refuses_an_address_for_its_own_attempts_alone() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();

    // a /24 that has made what it may: another address of it is refused, one of the next is not
    sign_in(&mut f, "198.51.100.1").unwrap();
    set(&f, &of("wider", "198.51.100.0"), 400);
    assert!(sign_in(&mut f, "198.51.100.2").is_err());
    sign_in(&mut f, "198.51.101.2").unwrap();
    // an IPv4 address written as an IPv6 one counts as the address it is
    assert!(sign_in(&mut f, "::ffff:198.51.100.3").is_err());
    sign_in(&mut f, "2001:db8:1::1").unwrap();
    set(&f, &of("wider", "2001:db8:1::"), 400);
    assert!(sign_in(&mut f, "2001:db8:1:ffff::1").is_err());
    sign_in(&mut f, "2001:db8:2::1").unwrap();

    // everybody together attempts more than a minute has the checking for: four
    // hundred and one sign-ins, each from an address and a network of its own
    let before = attempts(&f, "global");
    for host in 0..=400 - before {
        sign_in(&mut f, &format!("10.{}.{}.1", host / 256, host % 256)).unwrap();
    }
    assert_eq!(attempts(&f, "global"), 401);
    // an address nobody heard of is let in for the few that one person makes
    for attempt in 1..=5 {
        assert!(sign_in(&mut f, "203.0.113.5").is_ok(), "attempt {attempt}");
    }
    assert!(matches!(
        sign_in(&mut f, "203.0.113.5"),
        Err(Error::Attempts(_))
    ));
    assert!(sign_in(&mut f, "203.0.113.5").is_err());
    // and whoever has attempted little is let in still
    sign_in(&mut f, "203.0.113.6").unwrap();
    sign_in(&mut f, "198.51.101.2").unwrap();
}

/// A code is asked for without a credential and costs no password to be checked:
/// the codes asked for are held to what an address and its network may ask for,
/// and make nobody busy who signs in.
#[test]
#[ignore = "requires isolated PostgreSQL; the device codes asked for and the sign-ins of everybody"]
fn the_codes_asked_for_take_nothing_from_what_is_attempted_of_sign_ins() {
    let mut f = Fixture::new();
    f.store.enable_browser(&f.credential, &origin()).unwrap();
    sign_in(&mut f, "192.0.2.9").unwrap();
    let shared = attempts(&f, "global");
    // more codes than everybody may attempt of sign-ins before an address is
    // held to a few, from five addresses of two networks
    for asked in 0..420 {
        let network = format!("198.51.{}.{}", 100 + asked % 2, 1 + asked % 5);
        code(&mut f, &network).unwrap();
    }
    assert_eq!(attempts(&f, "global"), shared);
    assert_eq!(attempts(&f, &of("code", "198.51.100.1")), 42);
    // the sign-ins of an address that asked for codes are counted from none
    for attempt in 1..=7 {
        assert!(sign_in(&mut f, "198.51.100.1").is_ok(), "attempt {attempt}");
    }
    assert_eq!(attempts(&f, &of("network", "198.51.100.1")), 7);

    // an address is held to the codes it may ask for, and so is its network
    set(&f, &of("code", "198.51.100.1"), 100);
    match code(&mut f, "198.51.100.1") {
        Err(Error::Attempts(left)) => assert!((1..=60_000).contains(&left), "{left}"),
        other => panic!("{other:?}"),
    }
    code(&mut f, "198.51.100.2").unwrap();
    set(&f, &of("code-wider", "198.51.100.0"), 400);
    assert!(code(&mut f, "198.51.100.2").is_err());
    code(&mut f, "198.51.101.2").unwrap();
    // and none of that keeps the address from signing in
    sign_in(&mut f, "198.51.100.1").unwrap();
}
