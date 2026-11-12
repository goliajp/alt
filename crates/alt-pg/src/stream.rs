//! The socket and the receive buffer messages are parsed out of.

use crate::{Config, Error, Host, Result, wire::MAX_MESSAGE};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    ops::Range,
    os::unix::net::UnixStream,
};

pub(crate) enum Stream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Stream {
    pub fn open(config: &Config) -> Result<Self> {
        let port = config.port.unwrap_or(5432);
        let host = config.host.as_ref();
        if let Some(Host::Unix(directory)) = host {
            let path = directory.join(format!(".s.PGSQL.{port}"));
            return Ok(Self::Unix(UnixStream::connect(path)?));
        }
        let addresses: Vec<SocketAddr> = match (config.hostaddr, host) {
            (Some(ip), _) => vec![SocketAddr::new(ip, port)],
            (None, Some(Host::Tcp(name))) => (name.as_str(), port).to_socket_addrs()?.collect(),
            _ => return Err(Error::Config("host missing")),
        };
        let mut last = io::Error::new(io::ErrorKind::NotFound, "host has no address");
        for address in addresses {
            let attempt = match config.connect_timeout {
                Some(timeout) => TcpStream::connect_timeout(&address, timeout),
                None => TcpStream::connect(address),
            };
            match attempt {
                Ok(stream) => {
                    stream.set_nodelay(true)?;
                    return Ok(Self::Tcp(stream));
                }
                Err(error) => last = error,
            }
        }
        Err(last.into())
    }

    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.write_all(bytes),
            Self::Unix(stream) => stream.write_all(bytes),
        }
    }

    fn receive(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buffer),
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

const INITIAL: usize = 64 << 10;
/// a buffer grown past this by one large message is released afterwards
const KEEP: usize = 1 << 20;

pub(crate) struct Inbox {
    buffer: Vec<u8>,
    start: usize,
    end: usize,
}

impl Inbox {
    pub fn new() -> Self {
        Self {
            buffer: vec![0; INITIAL],
            start: 0,
            end: 0,
        }
    }

    /// the tag of the next message and where its body lies in the buffer
    pub fn next(&mut self, stream: &mut Stream) -> Result<(u8, Range<usize>)> {
        self.fill(stream, 5)?;
        let header = &self.buffer[self.start..self.start + 5];
        let length = u32::from_be_bytes(header[1..5].try_into().expect("four bytes")) as usize;
        if !(4..=MAX_MESSAGE).contains(&length) {
            return Err(Error::Protocol("message length out of range"));
        }
        self.fill(stream, 1 + length)?;
        let tag = self.buffer[self.start];
        let body = self.start + 5..self.start + 1 + length;
        self.start = body.end;
        Ok((tag, body))
    }

    pub fn bytes(&self, range: Range<usize>) -> &[u8] {
        &self.buffer[range]
    }

    /// called between responses, when nothing handed out is still in use
    pub fn relax(&mut self) {
        if self.start == self.end {
            self.start = 0;
            self.end = 0;
            if self.buffer.len() > KEEP {
                self.buffer = vec![0; INITIAL];
            }
        }
    }

    fn fill(&mut self, stream: &mut Stream, want: usize) -> Result<()> {
        if self.end - self.start >= want {
            return Ok(());
        }
        if self.start + want > self.buffer.len() {
            self.buffer.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
            if want > self.buffer.len() {
                self.buffer.resize(want.max(self.buffer.len() * 2), 0);
            }
        }
        while self.end - self.start < want {
            match stream.receive(&mut self.buffer[self.end..]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
                Ok(count) => self.end += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn pair() -> (Stream, UnixStream) {
        let (near, far) = UnixStream::pair().unwrap();
        (Stream::Unix(near), far)
    }

    #[test]
    fn messages_split_across_reads_come_out_whole() {
        let (mut stream, mut far) = pair();
        let writer = thread::spawn(move || {
            let mut bytes = Vec::new();
            for index in 0..2000u32 {
                bytes.push(b'D');
                bytes.extend_from_slice(&(4 + 100u32).to_be_bytes());
                bytes.extend_from_slice(&[index as u8; 100]);
            }
            for piece in bytes.chunks(7919) {
                far.write_all(piece).unwrap();
            }
        });
        let mut inbox = Inbox::new();
        for index in 0..2000u32 {
            let (tag, body) = inbox.next(&mut stream).unwrap();
            assert_eq!(tag, b'D');
            assert_eq!(inbox.bytes(body), [index as u8; 100]);
        }
        writer.join().unwrap();
    }

    #[test]
    fn a_message_larger_than_the_buffer_grows_it_and_is_released() {
        let (mut stream, mut far) = pair();
        let size = 3 << 20;
        let writer = thread::spawn(move || {
            far.write_all(b"Z\0\0\0\x05I").unwrap();
            far.write_all(b"D").unwrap();
            far.write_all(&(4 + size as u32).to_be_bytes()).unwrap();
            far.write_all(&vec![7; size]).unwrap();
        });
        let mut inbox = Inbox::new();
        assert_eq!(inbox.next(&mut stream).unwrap().0, b'Z');
        let (_, body) = inbox.next(&mut stream).unwrap();
        assert_eq!(body.len(), size);
        assert!(inbox.bytes(body).iter().all(|&byte| byte == 7));
        inbox.relax();
        assert_eq!(inbox.buffer.len(), INITIAL);
        writer.join().unwrap();
    }

    #[test]
    fn bad_lengths_and_early_ends_are_errors() {
        for bytes in [&b"D\0\0\0\x03"[..], b"D\xff\xff\xff\xff", b"D\x40\0\0\0"] {
            let (mut stream, mut far) = pair();
            far.write_all(bytes).unwrap();
            let error = Inbox::new().next(&mut stream).unwrap_err();
            assert!(matches!(error, Error::Protocol(_)), "{bytes:?}");
        }
        for bytes in [&b""[..], b"D\0\0", b"D\0\0\0\x08ab"] {
            let (mut stream, mut far) = pair();
            far.write_all(bytes).unwrap();
            drop(far);
            let error = Inbox::new().next(&mut stream).unwrap_err();
            assert!(matches!(error, Error::Io(_)), "{bytes:?}");
        }
    }
}
