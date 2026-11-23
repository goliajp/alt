//! A request as its handler has it: the head, the body as it comes off
//! the connection, and the answer that goes back on it.

use std::io::{self, Read};
use std::sync::mpsc::Sender;

use super::head::{Framing, Head, Header, Method};
use super::paced::{Sent, Source};
use super::response::Response;

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
    left: u64,
    chunks: Option<Chunks>,
    /// the peer is told to go on before the first of the body is read
    expects: bool,
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
                    let digits = size.split(|b| *b == b';').next().unwrap_or_default();
                    let size = std::str::from_utf8(digits).ok();
                    let size = size.and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok());
                    let size =
                        size.ok_or(io::Error::new(io::ErrorKind::InvalidData, "a chunk size"))?;
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
    /// the connection, for the next request on it, or nothing where it ended
    back: Sender<Option<Source>>,
}

impl Request {
    pub(super) fn new(head: Head, mut source: Source, back: Sender<Option<Source>>) -> Self {
        source.begin();
        let body = Body {
            source,
            left: match head.framing {
                Framing::Length(length) => length,
                Framing::Chunked => 0,
            },
            chunks: (head.framing == Framing::Chunked).then_some(Chunks::Size(false)),
            expects: head.expects,
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

    pub(crate) fn as_reader(&mut self) -> &mut dyn Read {
        self.body.as_mut().expect("a request not yet answered")
    }

    pub(crate) fn respond<R: Read>(mut self, response: Response<R>) -> io::Result<()> {
        self.answer(response)
    }

    /// Sends `response`. The connection goes on to its next request where
    /// the body of this one was read to its end; where it was not, what is
    /// left of it cannot be told from a next request, and the connection
    /// ends.
    fn answer<R: Read>(&mut self, response: Response<R>) -> io::Result<()> {
        let Some(body) = self.body.take() else {
            return Ok(());
        };
        let last = self.head.last || !body.over();
        let mut source = body.source;
        let bodiless = self.head.method == Method::Head;
        let sent = response.send(&mut source, last, bodiless);
        match (&sent, last) {
            (Ok(()), false) => drop(self.back.send(Some(source))),
            _ => {
                source.close();
                let _ = self.back.send(None);
            }
        }
        sent
    }
}

impl Drop for Request {
    /// A request its handler let go of without an answer is answered here.
    fn drop(&mut self) {
        let _ = self.answer(Response::from_data(Vec::new()).with_status_code(500));
    }
}
