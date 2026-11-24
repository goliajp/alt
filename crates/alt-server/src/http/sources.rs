//! The connections one address has in use at once.
//!
//! The connections there may be are few, and whoever opens them all keeps
//! everybody else out. So one address has no more than a part of them: counted
//! by connection where the address is the peer's own, and by request under way
//! where the peer is a proxy the configuration names, which stands for all its
//! clients and says in `X-Forwarded-For` whose each request is. A proxy that
//! says nothing of a request has no limit for it.

use super::{caller, receiving};
use axum::http::HeaderMap;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
};

pub(super) struct Sources {
    each: usize,
    proxies: Vec<caller::Network>,
    open: Mutex<HashMap<String, usize>>,
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

    /// The place of a connection `peer` opens: none to take where it is a proxy,
    /// and an error where its address has as many as one may.
    pub(super) fn connected(self: &Arc<Self>, peer: SocketAddr) -> Result<Option<Place>, ()> {
        match self.proxy(peer.ip()) {
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
        match self.proxy(peer.ip()) && !self.proxy(caller) {
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
        let proxies: Vec<caller::Network> = vec!["127.0.0.1".parse().unwrap()];
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
        drop(held);
        assert!(sources.connected(peer("203.0.113.7")).is_ok());

        // the proxy has no limit of its own, and each address it names has one
        let proxied: Vec<_> = (0..64)
            .map(|_| sources.connected(peer("127.0.0.1")))
            .collect();
        assert!(proxied.iter().all(|place| matches!(place, Ok(None))));
        let named = |from: &str| sources.asked(peer("127.0.0.1"), &forwarded(from));
        let under_way: Vec<_> = (0..2).map(|_| named("198.51.100.1").unwrap()).collect();
        assert!(under_way.iter().all(Option::is_some));
        assert!(named("198.51.100.1").is_err());
        assert!(named("198.51.100.2").unwrap().is_some());
        drop(under_way);
        assert!(named("198.51.100.1").unwrap().is_some());
        // a request the proxy names nobody for is the proxy's own
        assert!(
            sources
                .asked(peer("127.0.0.1"), &HeaderMap::new())
                .unwrap()
                .is_none()
        );
        assert!(sources.open.lock().unwrap().is_empty());
    }
}
