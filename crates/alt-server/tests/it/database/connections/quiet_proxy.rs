//! A proxy whose requests name nobody past it, most of the first: the log
//! says that a proxy in front is most likely not trusted, once.
use super::*;
use crate::database::sign_in_deadline::start;
use std::sync::{Arc, Mutex, OnceLock};

/// what the log of the process says, from the first server of the tests on
fn log() -> Arc<Mutex<Vec<u8>>> {
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        struct Kept(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Kept {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let kept = Arc::new(Mutex::new(Vec::new()));
        let logging = alt_server_telemetry::install_logging(Kept(kept.clone()), 4096).unwrap();
        std::mem::forget(logging);
        kept
    })
    .clone()
}

/// whether the log has said so of `proxy` within a few seconds
fn told_of(proxy: &str) -> bool {
    let said = format!("taken to come from the proxy at {proxy},");
    let until = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if String::from_utf8_lossy(&log().lock().unwrap()).contains(&said) {
            return true;
        }
        if std::time::Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn health(server: &Running, forwarded: &str) -> u16 {
    let asked = ureq::get(&format!("{}/healthz", server.url)).set("X-Forwarded-For", forwarded);
    match asked.timeout(Duration::from_secs(30)).call() {
        Ok(response) => response.status(),
        Err(ureq::Error::Status(status, _)) => status,
        Err(error) => panic!("{error}"),
    }
}

/// The proxy on this host passes on requests that a proxy it trusts says come
/// from that proxy: the callers are all taken for it. Once most of the first
/// ones are, the log says so, naming it. Where the proxy in front is trusted
/// and a hundred requests are sent to the proxy on this host by a client
/// beside it, named by nobody, before those of the callers come, it does not.
#[test]
#[ignore = "requires isolated PostgreSQL and loopback HTTP; exercised by server acceptance and closing gate"]
fn a_proxy_whose_requests_name_nobody_past_it_is_told_of_in_the_log() {
    log();
    let quiet = "192.0.2.77";
    let mut f = Fixture::new();
    let server = start(&mut f, |_, options| {
        options.trusted_proxies = vec!["127.0.0.1".parse().unwrap(), quiet.parse().unwrap()];
    });
    for _ in 0..260 {
        assert_eq!(health(&server, quiet), 200);
    }
    assert!(told_of(quiet));
    drop(server);

    let named = "192.0.2.78";
    let mut f = Fixture::new();
    let server = start(&mut f, |_, options| {
        options.trusted_proxies = vec!["127.0.0.1".parse().unwrap(), named.parse().unwrap()];
    });
    for _ in 0..100 {
        assert_eq!(health(&server, named), 200);
    }
    for caller in 0..200 {
        let forwarded = format!("203.0.113.{}, {named}", caller % 250);
        assert_eq!(health(&server, &forwarded), 200);
    }
    assert!(!told_of(named));
}
