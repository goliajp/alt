//! A relay between a client and `altd-server`: it passes on what each sends the
//! other, as slowly as it is told, and tells when a push comes.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

/// How a relay passes on what a client sends.
#[derive(Clone, Default)]
pub(crate) struct Relayed {
    /// the bytes of one piece and the rest after it, where the client's line is slow
    pub(crate) pace: Option<(usize, Duration)>,
    /// called when the request that brings a push comes, before it is passed on
    pub(crate) before_push: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Listens on a port of its own and relays each connection to `server`. The
/// address to ask instead of the server's. It serves until the test process ends.
pub(crate) fn relay(server: &str, relayed: Relayed) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = server.to_owned();
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let (Ok(client), Ok(upstream)) = (client, TcpStream::connect(&server)) else {
                break;
            };
            let relayed = relayed.clone();
            std::thread::spawn(move || one(client, upstream, &relayed));
        }
    });
    address
}

fn one(client: TcpStream, upstream: TcpStream, relayed: &Relayed) {
    let (mut answers, mut back) = (upstream.try_clone().unwrap(), client.try_clone().unwrap());
    // what the server answers goes back as it comes
    let answering = std::thread::spawn(move || {
        let _ = std::io::copy(&mut answers, &mut back);
        let _ = back.shutdown(Shutdown::Write);
    });
    let (mut from, mut to) = (client, upstream);
    let mut piece = vec![0u8; relayed.pace.map_or(16 * 1024, |(bytes, _)| bytes)];
    while let Ok(count @ 1..) = from.read(&mut piece) {
        let came = &piece[..count];
        // a request begins where a read does: its client waited for the answer
        // to the one before
        let push = came.starts_with(b"POST ")
            && came
                .windows(22)
                .any(|window| window == b"git-receive-pack HTTP/");
        if let (true, Some(before)) = (push, &relayed.before_push) {
            before();
        }
        if to.write_all(came).is_err() {
            break;
        }
        if let Some((_, rest)) = relayed.pace {
            std::thread::sleep(rest);
        }
    }
    let _ = to.shutdown(Shutdown::Write);
    let _ = answering.join();
}
