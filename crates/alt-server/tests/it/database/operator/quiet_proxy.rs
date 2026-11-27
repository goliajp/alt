//! A proxy whose requests name nobody past it, most of the first: the log
//! says that a proxy in front is most likely not trusted, once.
//!
//! The log read is that of a server process of its own. A process has one
//! log, and what it says is whole only where the log was there before the
//! first server of the process: a server of before it keeps on saying nothing,
//! and whatever it is the first to say nobody in the process says after it.
use super::*;
use std::time::Instant;

/// a server that trusts the proxy on this host and the one at `proxy`
fn trusting(proxy: &str) -> (OperatorDb, ObservedServer) {
    let db = OperatorDb::new();
    db.ok(&["secret", "receipt-key", "--output", &db.path("receipt.key")]);
    assert_eq!(db.ok(&["initialize"])["initialized"], true);
    let mut config = Config::read(&db.config).unwrap();
    config.http.trusted_proxies = vec!["127.0.0.1".into(), proxy.into()];
    std::fs::write(&db.config, alt_json::to_vec(&config).unwrap()).unwrap();
    let server = ObservedServer::start(&db);
    (db, server)
}

fn health(server: &ObservedServer, forwarded: &str) {
    let asked = ureq::get(&format!("{}/healthz", server.url)).set("X-Forwarded-For", forwarded);
    let answered = asked.timeout(Duration::from_secs(30)).call().unwrap();
    assert_eq!(answered.status(), 200);
}

/// Whether the log says so of `proxy`, read once it has all of `requests`:
/// it is written in the order it was said in, and a request is answered after
/// what is said of it, so that nothing said by then is still to come.
fn told_of(server: &ObservedServer, proxy: &str, requests: usize) -> bool {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let log = std::fs::read_to_string(&server.log).unwrap();
        if log.matches("\"request.started\"").count() >= requests {
            return log.contains(&format!("taken to come from the proxy at {proxy},"));
        }
        assert!(Instant::now() < until, "the log has not every request");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The proxy on this host passes on requests that a proxy it trusts says come
/// from that proxy: the callers are all taken for it. Once most of the first
/// ones are, the log says so, naming it. Where the proxy in front is trusted
/// and a hundred requests are sent to the proxy on this host by a client
/// beside it, named by nobody, before those of the callers come, it does not.
#[test]
#[ignore = "requires isolated PostgreSQL and a real server process; exercised by server acceptance and closing gate"]
fn a_proxy_whose_requests_name_nobody_past_it_is_told_of_in_the_log() {
    let quiet = "192.0.2.77";
    let (_db, server) = trusting(quiet);
    for _ in 0..260 {
        health(&server, quiet);
    }
    assert!(told_of(&server, quiet, 260));
    drop(server);

    let named = "192.0.2.78";
    let (_db, server) = trusting(named);
    for _ in 0..100 {
        health(&server, named);
    }
    for caller in 0..200 {
        health(&server, &format!("203.0.113.{}, {named}", caller % 250));
    }
    assert!(!told_of(&server, named, 300));
}
