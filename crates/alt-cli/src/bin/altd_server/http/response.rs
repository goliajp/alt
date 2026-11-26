//! The answer to a request: a status, headers and a body of known length.

use std::io::{self, Cursor, Read};

use super::head::Header;
use super::kept::piece;
use super::paced::{Sent, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusCode(pub(crate) u16);

impl From<u16> for StatusCode {
    fn from(code: u16) -> Self {
        StatusCode(code)
    }
}

pub(crate) struct Response<R: Read> {
    status: StatusCode,
    headers: Vec<Header>,
    body: R,
    length: u64,
}

impl Response<Cursor<Vec<u8>>> {
    pub(crate) fn from_data(data: impl Into<Vec<u8>>) -> Self {
        let data = data.into();
        Response {
            status: StatusCode(200),
            headers: Vec::new(),
            length: data.len() as u64,
            body: Cursor::new(data),
        }
    }

    pub(crate) fn from_string(text: impl Into<String>) -> Self {
        let mut response = Self::from_data(text.into().into_bytes());
        let plain = Header::from_bytes(b"Content-Type", b"text/plain; charset=UTF-8");
        response.headers.extend(plain);
        response
    }
}

impl<R: Read> Response<R> {
    /// `body`, which is `length` bytes read to its end.
    pub(crate) fn sized(length: u64, body: R) -> Self {
        Response {
            status: StatusCode(200),
            headers: Vec::new(),
            length,
            body,
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "",
    }
}

impl<R: Read> Response<R> {
    pub(crate) fn with_status_code(mut self, status: impl Into<StatusCode>) -> Self {
        self.status = status.into();
        self
    }

    pub(crate) fn add_header(&mut self, header: Header) {
        self.headers.push(header);
    }

    pub(crate) fn status_code(&self) -> StatusCode {
        self.status
    }

    pub(crate) fn data_length(&self) -> Option<usize> {
        usize::try_from(self.length).ok()
    }

    /// The response as it goes out, without its body where the request
    /// was for the head alone. `last` tells the peer that the connection
    /// ends behind it.
    pub(super) fn outgoing(self, last: bool, bodiless: bool) -> Outgoing
    where
        R: Send + 'static,
    {
        let status = self.status.0;
        let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
        for header in &self.headers {
            head.push_str(&format!("{}: {}\r\n", header.field.as_str(), header.value));
        }
        // a response of these statuses has no body, and says no length
        let empty = status < 200 || status == 204 || status == 304;
        if !empty {
            head.push_str(&format!("Content-Length: {}\r\n", self.length));
        }
        if last {
            head.push_str("Connection: close\r\n");
        }
        head.push_str("\r\n");
        Outgoing {
            status,
            head: head.into_bytes(),
            body: Box::new(self.body),
            length: if empty || bodiless { 0 } else { self.length },
            last,
        }
    }
}

/// A response on its way out: the bytes of its head, and its body.
pub(super) struct Outgoing {
    status: u16,
    head: Vec<u8>,
    body: Box<dyn Read + Send>,
    length: u64,
    /// the connection ends behind it
    pub(super) last: bool,
}

impl Outgoing {
    /// Writes the response to `source`, within the time a response of
    /// its length has.
    pub(super) fn send(mut self, source: &mut Source) -> io::Result<()> {
        let mut sent = Sent::new();
        source.send(&mut sent, &self.head)?;
        let mut piece = vec![0u8; 64 * 1024];
        let mut left = self.length;
        while left > 0 {
            let most = piece.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let read = self.body.read(&mut piece[..most])?;
            if read == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            source.send(&mut sent, &piece[..read])?;
            left -= read as u64;
        }
        Ok(())
    }

    /// Writes the rest of a response whose head and first `begun` bytes
    /// went out, in chunks where it is `chunked`, as the pieces behind them.
    /// A response of another status than 200 goes out as an error in the
    /// side band of a pack.
    pub(super) fn send_rest(
        mut self,
        source: &mut Source,
        begun: u64,
        chunked: bool,
    ) -> io::Result<()> {
        let mut sent = Sent::new();
        let mut out = Vec::new();
        if self.status == 200 {
            // the bytes that went out are the ones the body begins with
            io::copy(&mut (&mut self.body).take(begun), &mut io::sink())?;
            let mut read = vec![0u8; 64 * 1024];
            let mut left = self.length - begun;
            while left > 0 {
                let most = read.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                let count = self.body.read(&mut read[..most])?;
                if count == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                out.clear();
                piece(&mut out, &read[..count], chunked);
                source.send(&mut sent, &out)?;
                left -= count as u64;
            }
        } else {
            let mut said = b"\x03error: ".to_vec();
            (&mut self.body)
                .take(self.length.min(1000))
                .read_to_end(&mut said)?;
            let mut band = Vec::new();
            alt_wire::pkt::write_data(&mut band, &said)?;
            piece(&mut out, &band, chunked);
            source.send(&mut sent, &out)?;
        }
        match chunked {
            true => source.send(&mut sent, b"0\r\n\r\n"),
            false => Ok(()),
        }
    }
}
