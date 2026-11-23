//! The answer to a request: a status, headers and a body of known length.

use std::fs::File;
use std::io::{self, Cursor, Read};

use super::head::Header;
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

impl Response<File> {
    /// The file from where it is read to its end.
    pub(crate) fn from_file(file: File) -> Self {
        Response {
            status: StatusCode(200),
            headers: Vec::new(),
            length: file.metadata().map_or(0, |about| about.len()),
            body: file,
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

    /// Writes the response to `source`, without its body where the request
    /// was for the head alone. `last` tells the peer that the connection
    /// ends behind it.
    pub(super) fn send(
        mut self,
        source: &mut Source,
        last: bool,
        bodiless: bool,
    ) -> io::Result<()> {
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
        let mut sent = Sent::new();
        source.send(&mut sent, head.as_bytes())?;
        if empty || bodiless {
            return Ok(());
        }
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
}
