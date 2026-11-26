//! The connections one address has in use at once.
//!
//! The connections there may be are few, and whoever opens them all keeps
//! everybody else out. So one address has no more than a part of them: counted
//! by connection where the address is the peer's own, and by request under way
//! where the peer is a proxy the configuration names, which stands for all its
//! clients and says in `X-Forwarded-For` whose each request is. A proxy that
//! says nothing of a request has no limit for it, and neither has a peer on
//! this host: the proxy in front of an instance is one, named or not.
//!
//! Where the requests through a proxy name nobody past it, many in a row and
//! none named before, the log is told once: a proxy in front of it that names
//! the callers is most likely not one the configuration names, and every
//! caller is taken for that proxy.

use super::{caller, receiving};
use axum::http::HeaderMap;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering::Relaxed},
    },
};

/// the requests in a row through a proxy that name nobody past it after which
/// the log is told
const QUIET: usize = 32;

pub(super) struct Sources {
    each: usize,
    proxies: Vec<caller::Network>,
    open: Mutex<HashMap<String, usize>>,
    /// the requests through a proxy so far that named nobody past it; past
    /// [`QUIET`] once one named somebody
    quiet: AtomicUsize,
    log: alt_server_telemetry::BackgroundLog,
}

/// One connection, or one request under way, of an address: until it is dropped.
pub(super) struct Place {
    of: Arc<Sources>,
    address: String,
}

impl Sources {
    pub(super) fn new(each: usize, proxies: &[caller::Network]) -> Arc<Self> {
        Arc::new(Sources {
            each,
            proxies: proxies.to_vec(),
            open: Mutex::default(),
            quiet: AtomicUsize::new(0),
            log: alt_server_telemetry::BackgroundLog::current(),
        })
    }

    fn proxy(&self, peer: IpAddr) -> bool {
        self.proxies.iter().any(|proxy| proxy.contains(peer))
    }

    fn place(self: &Arc<Self>, address: IpAddr) -> Result<Place, ()> {
        let address = receiving::address(address);
        let mut open = self.open.lock().expect("sources");
        let count = open.entry(address.clone()).or_insert(0);
        if *count >= self.each {
            return Err(());
        }
        *count += 1;
        let of = self.clone();
        Ok(Place { of, address })
    }

    /// The place of a connection `peer` opens: none to take where it is a proxy or
    /// on this host, and an error where its address has as many as one may.
    pub(super) fn connected(self: &Arc<Self>, peer: SocketAddr) -> Result<Option<Place>, ()> {
        let here = match peer.ip() {
            IpAddr::V6(v6) => {
                v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
            }
            IpAddr::V4(v4) => v4.is_loopback(),
        };
        match here || self.proxy(peer.ip()) {
            true => Ok(None),
            false => self.place(peer.ip()).map(Some),
        }
    }

    /// The place of a request that came over the connection of `peer`: none to
    /// take where the connection has its own or the proxy names nobody.
    pub(super) fn asked(
        self: &Arc<Self>,
        peer: SocketAddr,
        headers: &HeaderMap,
    ) -> Result<Option<Place>, ()> {
        let caller = caller::caller(peer, headers, &self.proxies);
        let (proxy, named) = (self.proxy(peer.ip()), !self.proxy(caller));
        if proxy && self.quiet.load(Relaxed) < QUIET {
            if named {
                self.quiet.store(usize::MAX, Relaxed);
            } else if self.quiet.fetch_add(1, Relaxed) + 1 == QUIET {
                self.log
                    .failed("configuration", "", &caller::nobody(caller));
            }
        }
        match proxy && named {
            true => self.place(caller).map(Some),
            false => Ok(None),
        }
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let mut open = self.of.open.lock().expect("sources");
        if let Some(count) = open.get_mut(&self.address) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.address);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(address: &str) -> SocketAddr {
        format!("{address}:4000").parse().unwrap()
    }

    fn forwarded(from: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", from.parse().unwrap());
        headers
    }

    #[test]
    fn an_address_has_so_many_connections_in_use_whether_it_is_a_peer_or_named_by_a_proxy() {
        let proxies: Vec<caller::Network> = vec!["192.0.2.1".parse().unwrap()];
        let sources = Sources::new(2, &proxies);
        // a peer that is no proxy, by connection
        let held: Vec<_> = (0..2)
            .map(|_| sources.connected(peer("203.0.113.7")).unwrap().unwrap())
            .collect();
        assert!(sources.connected(peer("203.0.113.7")).is_err());
        assert!(sources.connected(peer("203.0.113.8")).unwrap().is_some());
        // its requests take no second place
        let own = sources.asked(peer("203.0.113.7"), &forwarded("198.51.100.1"));
        assert!(own.unwrap().is_none());
        // the hosts of one IPv6 network are one address
        let six: Vec<_> = (1..=2)
            .map(|host| sources.connected(peer(&format!("[2001:db8::{host}]"))))
            .collect();
        assert!(six.iter().all(|place| place.is_ok()));
        assert!(sources.connected(peer("[2001:db8::9]")).is_err());
        assert!(sources.connected(peer("[2001:db8:0:1::9]")).is_ok());
        drop((held, six));
        assert!(sources.connected(peer("203.0.113.7")).is_ok());

        // the proxy has no limit of its own, nor has a peer on this host, and
        // each address the proxy names has one
        for unlimited in ["192.0.2.1", "127.0.0.1", "[::1]", "[::ffff:127.0.0.1]"] {
            let open: Vec<_> = (0..64)
                .map(|_| sources.connected(peer(unlimited)))
                .collect();
            assert!(open.iter().all(|place| matches!(place, Ok(None))));
        }
        // a peer on this host that is no proxy names nobody
        let own = sources.asked(peer("127.0.0.1"), &forwarded("198.51.100.1"));
        assert!(own.unwrap().is_none());
        let named = |from: &str| sources.asked(peer("192.0.2.1"), &forwarded(from));
        let under_way: Vec<_> = (0..2).map(|_| named("198.51.100.1").unwrap()).collect();
        assert!(under_way.iter().all(Option::is_some));
        assert!(named("198.51.100.1").is_err());
        assert!(named("198.51.100.2").unwrap().is_some());
        drop(under_way);
        assert!(named("198.51.100.1").unwrap().is_some());
        // a request the proxy names nobody for is the proxy's own
        assert!(
            sources
                .asked(peer("192.0.2.1"), &HeaderMap::new())
                .unwrap()
                .is_none()
        );
        assert!(sources.open.lock().unwrap().is_empty());
    }
}
