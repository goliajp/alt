//! The bodies of requests while they are received: no worker is held for one.
//!
//! A worker is what a request runs on, and there are few. A body that is slow to
//! come would keep one for as long as a body is given, so a request has none while
//! its body is received: it had one to be authorized, gave it back, and waits for
//! one again when the body is whole. That wait is the request's own, up to the time
//! a request may run: a push that was sent in full is not turned away because the
//! instance happened to be busy at that moment.
//!
//! What is bounded instead is the receiving itself: how many bodies one caller has
//! under way at once, and how many bytes of bodies the instance holds that no
//! worker has taken yet, of which the bodies of one caller have a quarter between
//! them: more only where the caller has one body alone. A caller is an account,
//! or an address where nobody has signed in.

use super::*;
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::OwnedSemaphorePermit;

pub(super) struct Receiving {
    /// by caller: the bodies under way, and the bytes they hold between them
    callers: Mutex<HashMap<String, (usize, u64)>>,
    bytes: AtomicU64,
    each: usize,
    most: u64,
}

/// why a body is not received
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Full {
    /// the caller has as many bodies under way as one caller may
    Caller,
    /// the instance holds as many bytes of bodies as it may
    Instance,
}

/// One body being received, until it is dropped.
pub(super) struct Slot {
    of: Arc<Receiving>,
    caller: String,
    bytes: u64,
}

impl Receiving {
    pub(super) fn new(each: usize, most: u64) -> Arc<Self> {
        Arc::new(Receiving {
            callers: Mutex::default(),
            bytes: AtomicU64::new(0),
            each,
            most,
        })
    }

    /// Begins a body of `caller` that says it has `declared` bytes.
    pub(super) fn begin(self: &Arc<Self>, caller: &str, declared: u64) -> Result<Slot, Full> {
        if self.bytes.load(Ordering::Acquire).saturating_add(declared) > self.most {
            return Err(Full::Instance);
        }
        let mut callers = self.callers.lock().expect("receiving callers");
        let (under_way, held) = callers.entry(caller.to_owned()).or_insert((0, 0));
        let beside = *under_way > 0 && held.saturating_add(declared) > self.most / 4;
        if *under_way >= self.each || beside {
            if *under_way == 0 {
                callers.remove(caller);
            }
            return Err(Full::Caller);
        }
        *under_way += 1;
        Ok(Slot {
            of: self.clone(),
            caller: caller.to_owned(),
            bytes: 0,
        })
    }
}

impl Slot {
    /// Counts `bytes` more of the body.
    pub(super) fn took(&mut self, bytes: u64) -> Result<(), Full> {
        self.bytes += bytes;
        let held = self.of.bytes.fetch_add(bytes, Ordering::AcqRel);
        let mut callers = self.of.callers.lock().expect("receiving callers");
        let (under_way, theirs) = callers.get_mut(&self.caller).expect("a body under way");
        *theirs += bytes;
        if *under_way > 1 && *theirs > self.of.most / 4 {
            return Err(Full::Caller);
        }
        if held.saturating_add(bytes) > self.of.most {
            return Err(Full::Instance);
        }
        Ok(())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.of.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        let mut callers = self.of.callers.lock().expect("receiving callers");
        if let Some((under_way, held)) = callers.get_mut(&self.caller) {
            *under_way -= 1;
            *held -= self.bytes;
            if *under_way == 0 {
                callers.remove(&self.caller);
            }
        }
    }
}

/// the response to a body that is not received for `full`
pub(super) fn refusal(full: Full, key: Option<RequestKey>) -> Response<ResponseBody> {
    let (status, message) = match full {
        Full::Caller => (
            429,
            "Too many request bodies are being received from this caller",
        ),
        Full::Instance => (
            503,
            "Instance is receiving as much as it holds; retry the same request",
        ),
    };
    let code = ErrorCode::TemporarilyUnavailable;
    failure(status, code, message, key, Retry::SameRequest)
}

/// the response to a body that could not be received
pub(super) fn unreceived(error: BodyError, key: Option<RequestKey>) -> Response<ResponseBody> {
    let unavailable = ErrorCode::TemporarilyUnavailable;
    let (status, code, message, retry) = match error {
        BodyError::Limit => (
            413,
            ErrorCode::QuotaExceeded,
            "Request body exceeds the selected bound",
            Retry::Never,
        ),
        BodyError::Storage => (
            503,
            unavailable,
            "Request storage is temporarily unavailable",
            Retry::SameRequest,
        ),
        BodyError::Malformed => (
            400,
            ErrorCode::InvalidRequest,
            "Request body is incomplete or contains unsupported trailers",
            Retry::SameRequest,
        ),
        BodyError::Slow => (
            408,
            unavailable,
            "Request body deadline exceeded",
            Retry::SameRequest,
        ),
        BodyError::Full(full) => return refusal(full, key),
    };
    failure(status, code, message, key, retry)
}

/// The key of a caller nobody has signed in as: its address, a /64 for IPv6.
pub(super) fn address(caller: std::net::IpAddr) -> String {
    match caller {
        // every host of the link the server is on has the same first 64 bits
        std::net::IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => {
            format!("address:{v6}")
        }
        std::net::IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => {
            format!("address:{:x}", u128::from(v6) >> 64)
        }
        other => format!("address:{other}"),
    }
}

/// The body of the request of `caller`, of `size` bytes at most, and the worker
/// the request then runs on: received with no worker held, and one waited for
/// once it is whole.
pub(super) async fn receive(
    state: &App,
    request: Request,
    caller: &str,
    size: u64,
    key: &Option<RequestKey>,
) -> Result<(alt_os::temp::NamedTempFile, OwnedSemaphorePermit), Box<Response<ResponseBody>>> {
    let length = request.headers().get(header::CONTENT_LENGTH);
    let declared = length.map(|length| length.to_str().ok().and_then(|s| s.parse::<u64>().ok()));
    if declared.is_some_and(|declared| declared.is_none_or(|n| n > size)) {
        return Err(Box::new(unreceived(BodyError::Limit, key.clone())));
    }
    let begun = state
        .receiving
        .begin(caller, declared.flatten().unwrap_or(0));
    let mut slot = begun.map_err(|full| Box::new(refusal(full, key.clone())))?;
    let receipt = body::Receipt::of(state, Some(&mut slot));
    let spooled = spool(request.into_body(), size, receipt).await;
    let source = spooled.map_err(|error| Box::new(unreceived(error, key.clone())))?;
    // whole: from here the request waits for a worker like any that runs
    let permit = worker(state, key.clone()).await?;
    Ok((source, permit))
}

/// A worker for a request whose body is whole: waited for as long as a request may
/// run, and no longer. Nothing of the request was done where none came.
pub(super) async fn worker(
    state: &App,
    key: Option<RequestKey>,
) -> Result<OwnedSemaphorePermit, Box<Response<ResponseBody>>> {
    let waited = state.workers.clone().acquire_owned();
    match tokio::time::timeout(state.options.work_timeout, waited).await {
        Ok(Ok(permit)) => Ok(permit),
        _ => Err(Box::new(failure(
            503,
            ErrorCode::TemporarilyUnavailable,
            "Instance is busy; retry the same request",
            key,
            Retry::SameRequest,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caller_has_so_many_bodies_under_way_and_the_instance_so_many_bytes() {
        let receiving = Receiving::new(2, 1000);
        let first = receiving.begin("a", 0).unwrap();
        let mut second = receiving.begin("a", 200).unwrap();
        assert_eq!(receiving.begin("a", 0).err(), Some(Full::Caller));
        let mut other = receiving.begin("b", 0).unwrap();
        // what a body says it has is looked at, and what comes of it is counted
        second.took(200).unwrap();
        assert_eq!(receiving.begin("c", 801).err(), Some(Full::Instance));
        other.took(800).unwrap();
        assert_eq!(other.took(1), Err(Full::Instance));
        drop(other);
        // a body that is over gives back its place and its bytes
        drop(first);
        let mut third = receiving.begin("a", 50).unwrap();
        third.took(50).unwrap();
        drop((second, third));
        assert!(receiving.callers.lock().unwrap().is_empty());
        assert_eq!(receiving.bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn the_bodies_of_one_caller_have_a_quarter_of_the_bytes_between_them() {
        let receiving = Receiving::new(8, 1000);
        // one body alone may be larger than that
        let mut alone = receiving.begin("a", 0).unwrap();
        alone.took(300).unwrap();
        // a second beside it is not begun for what it says, nor goes on past the quarter
        assert_eq!(receiving.begin("a", 1).err(), Some(Full::Caller));
        drop(alone);
        let mut slow: Vec<Slot> = (0..4).map(|_| receiving.begin("a", 0).unwrap()).collect();
        for slot in &mut slow[..3] {
            slot.took(80).unwrap();
        }
        assert_eq!(slow[3].took(11), Err(Full::Caller));
        // the others have what the instance holds beyond that
        let mut other = receiving.begin("b", 700).unwrap();
        other.took(700).unwrap();
        drop((slow, other));
        assert!(receiving.callers.lock().unwrap().is_empty());
        assert_eq!(receiving.bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn the_addresses_of_one_host_are_one_caller() {
        let of = |text: &str| address(text.parse().unwrap());
        assert_eq!(of("2001:db8::1"), of("2001:db8::ffff"));
        assert_ne!(of("2001:db8::1"), of("2001:db8:0:1::1"));
        assert_ne!(of("192.0.2.1"), of("192.0.2.2"));
        assert_ne!(of("fe80::1"), of("fe80::2"));
    }
}
