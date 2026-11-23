//! A request as its handler has it: the head, the body as it comes off
//! the connection, and the answer that goes back on it.
//!
//! A worker waits for the peer in one place, the reads of the body, and
//! there within the time the body has (see [`super::paced`]), which
//! begins with the first of those reads: what the request waited for a
//! worker is not the peer's. The answer is handed back to the thread of
//! the connection, which writes it and ends the connection where it
//! has to; the worker is on to its next request by then.

use std::io::{self, Read};
use std::sync::mpsc::Sender;

use super::head::{Framing, Head, Header, Method};
use super::paced::{Sent, Source};
use super::response::{Outgoing, Response};

/// Where the reading of a chunked body is.
enum Chunks {
    /// before the size of a chunk; `true` behind the bytes of one
    Size(bool),
    /// this many bytes of a chunk are still to come
    Within(u64),
    Over,
}

/// The body of a request, read off the connection as it is asked for.
struct Body {
    source: Source,
    /// the bytes on the wire it may have
    most: u64,
    left: u64,
    chunks: Option<Chunks>,
    /// the peer is told to go on before the first of the body is read
    expects: bool,
    /// not read from yet
    untouched: bool,
}

fn line(source: &mut Source) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        match source.byte()? {
            b'\n' => break,
            byte if line.len() < 4096 => line.push(byte),
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "a chunk line")),
        }
    }
    if line.pop() != Some(b'\r') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a chunk line"));
    }
    Ok(line)
}

/// The size a chunk line gives: hex digits and nothing else, before any
/// extensions.
fn chunk_size(line: &[u8]) -> Option<u64> {
    let digits = line.iter().take_while(|b| b.is_ascii_hexdigit()).count();
    let behind = line[digits..].trim_ascii_start();
    let extended = behind.is_empty() == (digits == line.len());
    if digits == 0 || digits > 16 || !extended || !(behind.is_empty() || behind[0] == b';') {
        return None;
    }
    u64::from_str_radix(std::str::from_utf8(&line[..digits]).ok()?, 16).ok()
}

impl Body {
    fn over(&self) -> bool {
        match &self.chunks {
            Some(chunks) => matches!(chunks, Chunks::Over),
            None => self.left == 0,
        }
    }

    /// The bytes still to come of the chunk the body is in, having read up
    /// to them. Zero at the end of the body.
    fn chunk(&mut self) -> io::Result<u64> {
        loop {
            match self.chunks.as_ref().expect("chunked") {
                Chunks::Over => return Ok(0),
                Chunks::Within(left) if *left > 0 => return Ok(*left),
                Chunks::Within(_) => self.chunks = Some(Chunks::Size(true)),
                Chunks::Size(behind) => {
                    if *behind && !line(&mut self.source)?.is_empty() {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "a chunk end"));
                    }
                    let size = line(&mut self.source)?;
                    let size = chunk_size(&size)
                        .ok_or(io::Error::new(io::ErrorKind::InvalidData, "a chunk size"))?;
                    if size > 0 {
                        self.chunks = Some(Chunks::Within(size));
                        continue;
                    }
                    // the trailers, to the empty line behind them
                    while !line(&mut self.source)?.is_empty() {}
                    self.chunks = Some(Chunks::Over);
                }
            }
        }
    }
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.over() || buf.is_empty() {
            return Ok(0);
        }
        if std::mem::take(&mut self.untouched) {
            self.source.begin_body(self.most);
        }
        if std::mem::take(&mut self.expects) {
            let go_on = b"HTTP/1.1 100 Continue\r\n\r\n";
            self.source.send(&mut Sent::new(), go_on)?;
        }
        let left = match self.chunks {
            Some(_) => self.chunk()?,
            None => self.left,
        };
        if left == 0 {
            return Ok(0);
        }
        let most = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let read = self.source.read(&mut buf[..most])?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        match &mut self.chunks {
            Some(Chunks::Within(left)) => *left -= read as u64,
            _ => self.left -= read as u64,
        }
        Ok(read)
    }
}

pub(crate) struct Request {
    head: Head,
    /// nothing once the request is answered
    body: Option<Body>,
    /// the connection and what is to be written on it
    back: Sender<(Source, Outgoing)>,
}

impl Request {
    pub(super) fn new(head: Head, source: Source, back: Sender<(Source, Outgoing)>) -> Self {
        let body = Body {
            source,
            most: u64::MAX,
            left: match head.framing {
                Framing::Length(length) => length,
                Framing::Chunked => 0,
            },
            chunks: (head.framing == Framing::Chunked).then_some(Chunks::Size(false)),
            expects: head.expects,
            untouched: true,
        };
        Request {
            head,
            body: Some(body),
            back,
        }
    }

    pub(crate) fn url(&self) -> &str {
        &self.head.url
    }

    pub(crate) fn method(&self) -> &Method {
        &self.head.method
    }

    pub(crate) fn headers(&self) -> &[Header] {
        &self.head.headers
    }

    /// The length the request says its body has.
    pub(crate) fn body_length(&self) -> Option<usize> {
        match self.head.framing {
            Framing::Length(length) => usize::try_from(length).ok(),
            Framing::Chunked => None,
        }
    }

    /// The body, which may take `most` bytes on the wire, framing and
    /// all: reading past them is an error. The first call decides.
    pub(crate) fn body(&mut self, most: u64) -> &mut dyn Read {
        let body = self.body.as_mut().expect("a request not yet answered");
        if body.untouched {
            body.most = body.most.min(most);
        }
        body
    }

    /// Has `response` sent. Whether the peer takes it is not waited for.
    pub(crate) fn respond<R: Read + Send + 'static>(mut self, response: Response<R>) {
        self.answer(response);
    }

    /// Hands `response` to the thread of the connection. The connection
    /// goes on to its next request where the body of this one was read
    /// to its end; where it was not, what is left of it cannot be told
    /// from a next request, and the connection ends.
    fn answer<R: Read + Send + 'static>(&mut self, response: Response<R>) {
        let Some(body) = self.body.take() else {
            return;
        };
        let last = self.head.last || !body.over();
        let bodiless = self.head.method == Method::Head;
        // the thread is gone where its connection is
        let _ = self
            .back
            .send((body.source, response.outgoing(last, bodiless)));
    }
}

impl Drop for Request {
    /// A request its handler let go of without an answer is answered here.
    fn drop(&mut self) {
        self.answer(Response::from_data(Vec::new()).with_status_code(500));
    }
}

#[cfg(test)]
mod tests {
    use super::chunk_size;

    #[test]
    fn the_size_of_a_chunk_is_hex_digits_and_nothing_before_or_around_them() {
        for (line, size) in [
            ("0", 0),
            ("1a", 26),
            ("A;x=y", 10),
            ("3 ;x", 3),
            ("ffffffffffffffff", u64::MAX),
        ] {
            assert_eq!(chunk_size(line.as_bytes()), Some(size), "{line:?}");
        }
        for line in [
            "",
            "+3",
            " 3",
            "\t3",
            "3 ",
            "3\t",
            "0x3",
            "3 x",
            ";x",
            "-1",
            "10000000000000000",
        ] {
            assert_eq!(chunk_size(line.as_bytes()), None, "{line:?}");
        }
    }
}
