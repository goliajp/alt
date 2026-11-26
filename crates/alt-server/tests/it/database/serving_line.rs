//! A line to the database that a test makes go silent, or holds up the
//! session of the lock on.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A line to the database a test can make go silent: each connection made to
/// it goes on to the socket of the database, and those made before
/// [`Line::silent`] carry nothing more either way, without being closed. The
/// connection that asked for the lock of the serving process carries what it
/// is sent only once it is let go on while [`Line::held_up`] says so. Once the
/// line is dropped they are closed.
pub(super) struct Line {
    pub(super) address: std::net::SocketAddr,
    /// how many connections were made, and how many of them carry nothing more
    made: Arc<(AtomicUsize, AtomicUsize)>,
    clients: Arc<Mutex<Vec<std::net::TcpStream>>>,
    /// the connection that asked for the lock, counted from one
    session: Arc<AtomicUsize>,
    held: Arc<AtomicBool>,
}

impl Line {
    pub(super) fn to(socket: std::path::PathBuf) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let made = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let (session, held) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let (counts, kept) = (made.clone(), clients.clone());
        let (asked, holding) = (session.clone(), held.clone());
        std::thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(client) = client else { return };
                let number = counts.0.fetch_add(1, SeqCst);
                let database = std::os::unix::net::UnixStream::connect(&socket).unwrap();
                kept.lock().unwrap().push(client.try_clone().unwrap());
                let ends: [(Box<dyn Read + Send>, Box<dyn Write + Send>); 2] = [
                    (
                        Box::new(client.try_clone().unwrap()),
                        Box::new(database.try_clone().unwrap()),
                    ),
                    (Box::new(database), Box::new(client)),
                ];
                for (toward, (mut from, mut to)) in ends.into_iter().enumerate() {
                    let counts = counts.clone();
                    let (asked, holding) = (asked.clone(), holding.clone());
                    std::thread::spawn(move || {
                        let mut buffer = vec![0; 64 << 10];
                        while let Ok(read @ 1..) = from.read(&mut buffer) {
                            if counts.1.load(SeqCst) > number {
                                return;
                            }
                            let lock: &[u8] = b"pg_advisory_lock(";
                            if toward == 0 && buffer[..read].windows(lock.len()).any(|w| w == lock)
                            {
                                asked.store(number + 1, SeqCst);
                            }
                            while holding.load(SeqCst) && asked.load(SeqCst) == number + 1 {
                                std::thread::sleep(Duration::from_millis(20));
                            }
                            if to.write_all(&buffer[..read]).is_err() {
                                return;
                            }
                        }
                    });
                }
            }
        });
        Line {
            address,
            made,
            clients,
            session,
            held,
        }
    }

    /// While `held` says so, what the session that asked for the lock is sent
    /// waits on the line, either way, and goes on once it is let go of.
    pub(super) fn held_up(&self, held: bool) {
        assert!(
            self.session.load(SeqCst) > 0,
            "no session asked for the lock"
        );
        self.held.store(held, SeqCst);
    }

    pub(super) fn silent(&self) {
        self.made.1.store(self.made.0.load(SeqCst), SeqCst);
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        for client in self.clients.lock().unwrap().iter() {
            let _ = client.shutdown(std::net::Shutdown::Both);
        }
    }
}
