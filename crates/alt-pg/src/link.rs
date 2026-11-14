//! One connection: requests go out in a single write, responses are read up
//! to the server's `ReadyForQuery`.

use crate::{
    Column, Error, Oid, Result, ToSql,
    row::RowSet,
    statement::Statement,
    stream::{Inbox, Stream},
    types::write_value,
    wire::{self, Body},
};
use std::sync::Arc;

/// where the interesting part of a response goes
pub(crate) enum Sink<'a> {
    Discard,
    Rows(&'a mut RowSet),
    Description(&'a mut Described),
}

#[derive(Default)]
pub(crate) struct Described {
    params: Option<Box<[Oid]>>,
    columns: Option<Arc<[Column]>>,
}

pub(crate) struct Link {
    pub(crate) stream: Stream,
    pub(crate) inbox: Inbox,
    pub(crate) out: Vec<u8>,
    /// statements dropped from the cache that the server still holds
    pub(crate) closing: Vec<u32>,
    /// the transaction status byte of the last `ReadyForQuery`
    pub(crate) status: u8,
    /// a transaction block is opened together with the next request
    pub(crate) begin: bool,
    pub(crate) broken: bool,
}

impl Link {
    /// starts an extended-protocol batch with the work that rides along
    fn start(&mut self) -> Result<()> {
        if self.broken {
            return Err(Error::Closed);
        }
        self.out.clear();
        for &id in &self.closing {
            wire::close_statement(&mut self.out, id)?;
        }
        if self.begin {
            wire::begin_transaction(&mut self.out);
        }
        Ok(())
    }

    fn send(&mut self) -> Result<()> {
        if let Err(error) = self.stream.send(&self.out) {
            self.broken = true;
            return Err(error.into());
        }
        self.closing.clear();
        self.begin = false;
        Ok(())
    }

    pub fn prepare(&mut self, sql: &str, id: u32) -> Result<Statement> {
        self.start()?;
        wire::parse(&mut self.out, id, sql)?;
        wire::describe_statement(&mut self.out, id)?;
        wire::sync(&mut self.out);
        self.send()?;
        let mut described = Described::default();
        self.read(1, 0, Sink::Description(&mut described))?;
        match (described.params, described.columns) {
            (Some(params), Some(columns)) => Ok(Statement {
                id,
                params,
                columns,
            }),
            _ => {
                self.broken = true;
                Err(Error::Protocol("statement was not described"))
            }
        }
    }

    /// Binds and runs a prepared statement in one round trip. The statements of
    /// `around` take no parameters and run before and after it in the same request;
    /// their results are dropped.
    pub fn execute(
        &mut self,
        statement: &Statement,
        params: &[&(dyn ToSql + Sync)],
        around: Option<(&Statement, &Statement)>,
        sink: Sink<'_>,
    ) -> Result<u64> {
        if params.len() != statement.params.len() {
            return Err(Error::ParameterCount {
                expected: statement.params.len(),
                given: params.len(),
            });
        }
        self.start()?;
        // the commands answered before this statement's: an opening BEGIN, the one before
        let answer = u32::from(self.begin) + u32::from(around.is_some());
        if let Some((before, _)) = around {
            wire::run_plain(&mut self.out, before.id);
        }
        let at = wire::begin(&mut self.out, b'B');
        self.out.push(0);
        wire::statement_name(&mut self.out, statement.id);
        // one format code for all parameters: binary
        self.out.extend_from_slice(&[0, 1, 0, 1]);
        self.out
            .extend_from_slice(&(params.len() as u16).to_be_bytes());
        for (index, (value, &ty)) in params.iter().zip(&statement.params).enumerate() {
            write_value(value, ty, &mut self.out).map_err(|reason| Error::Parameter {
                index,
                ty,
                reason,
            })?;
        }
        // one format code for all result columns: binary
        self.out.extend_from_slice(&[0, 1, 0, 1]);
        wire::finish(&mut self.out, at)?;
        wire::execute(&mut self.out);
        if let Some((_, after)) = around {
            wire::run_plain(&mut self.out, after.id);
        }
        wire::sync(&mut self.out);
        self.send()?;
        self.read(1, answer, sink)
    }

    /// runs statements through the simple protocol and discards their rows
    pub fn simple(&mut self, sql: &str) -> Result<()> {
        if self.broken {
            return Err(Error::Closed);
        }
        self.out.clear();
        let mut groups = 1;
        if !self.closing.is_empty() {
            for &id in &self.closing {
                wire::close_statement(&mut self.out, id)?;
            }
            wire::sync(&mut self.out);
            groups = 2;
        }
        if self.begin {
            // in one message a failed BEGIN also skips the statements
            wire::query(&mut self.out, &["BEGIN;", sql])?;
        } else {
            wire::query(&mut self.out, &[sql])?;
        }
        self.send()?;
        self.read(groups, 0, Sink::Discard).map(drop)
    }

    /// Reads `groups` responses; the first server error is returned after the last
    /// of them. Rows and the row count are those of the command at `answer`,
    /// counting the commands that completed from zero.
    pub(crate) fn read(&mut self, groups: u32, answer: u32, sink: Sink<'_>) -> Result<u64> {
        let mut failure = None;
        match self.read_messages(groups, answer, sink, &mut failure) {
            Ok(affected) => {
                self.inbox.relax();
                match failure {
                    Some(error) => Err(Error::Server(error)),
                    None => Ok(affected),
                }
            }
            Err(error) => {
                self.broken = true;
                // a fatal error is followed by the server hanging up
                Err(failure.map_or(error, Error::Server))
            }
        }
    }

    fn read_messages(
        &mut self,
        mut groups: u32,
        answer: u32,
        mut sink: Sink<'_>,
        failure: &mut Option<Box<crate::ServerError>>,
    ) -> Result<u64> {
        let mut affected = 0;
        let mut completed = 0;
        loop {
            let (tag, range) = self.inbox.next(&mut self.stream)?;
            let body = self.inbox.bytes(range);
            match tag {
                b'D' => {
                    if let Sink::Rows(rows) = &mut sink
                        && completed == answer
                    {
                        rows.push(body)?;
                    }
                }
                b'C' => {
                    if completed == answer {
                        affected = wire::affected(body)?;
                    }
                    completed += 1;
                }
                b'Z' => {
                    let &[status @ (b'I' | b'T' | b'E')] = body else {
                        return Err(Error::Protocol("transaction status"));
                    };
                    self.status = status;
                    groups -= 1;
                    if groups == 0 {
                        return Ok(affected);
                    }
                }
                b'E' => {
                    let error = wire::server_error(body)?;
                    failure.get_or_insert_with(|| Box::new(error));
                }
                b't' => {
                    if let Sink::Description(described) = &mut sink {
                        described.params = Some(parameter_types(body)?);
                    }
                }
                b'T' => {
                    if let Sink::Description(described) = &mut sink {
                        described.columns = Some(columns(body)?);
                    }
                }
                b'n' => {
                    if let Sink::Description(described) = &mut sink {
                        described.columns = Some(Arc::from(Vec::new()));
                    }
                }
                // parse, bind and close completion, empty query, and the
                // messages a server may send at any time: notice, parameter
                // status, notification
                b'1' | b'2' | b'3' | b'I' | b'N' | b'S' | b'A' => {}
                b'G' | b'H' | b'W' => return Err(Error::Protocol("COPY is not supported")),
                _ => return Err(Error::Protocol("unexpected message")),
            }
        }
    }
}

fn parameter_types(body: &[u8]) -> Result<Box<[Oid]>> {
    let mut body = Body::new(body);
    let count = u16::from_be_bytes(body.take(2)?.try_into().expect("two bytes"));
    let types = (0..count).map(|_| body.u32()).collect::<Result<_>>()?;
    body.end()?;
    Ok(types)
}

fn columns(body: &[u8]) -> Result<Arc<[Column]>> {
    let mut body = Body::new(body);
    let count = u16::from_be_bytes(body.take(2)?.try_into().expect("two bytes"));
    let mut columns = Vec::with_capacity(usize::from(count).min(body.remaining() / 19));
    for _ in 0..count {
        let name = body.text()?;
        // table oid and attribute number
        body.take(6)?;
        let ty = body.u32()?;
        // type length, type modifier, format
        body.take(8)?;
        columns.push(Column::new(name, ty));
    }
    body.end()?;
    Ok(columns.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oid;
    use std::{io::Write, os::unix::net::UnixStream};

    /// a link whose peer already wrote `response` and hung up
    fn answered(response: &[u8]) -> Link {
        let (near, mut far) = UnixStream::pair().unwrap();
        far.write_all(response).unwrap();
        Link {
            stream: Stream::Unix(near),
            inbox: Inbox::new(),
            out: Vec::new(),
            closing: Vec::new(),
            status: b'I',
            begin: false,
            broken: false,
        }
    }

    const ERROR: &[u8] = b"E\0\0\0\x1dSERROR\0C22012\0Mdivision\0\0";
    const READY: &[u8] = b"Z\0\0\0\x05I";

    #[test]
    fn an_error_is_returned_once_the_server_is_ready_again() {
        let mut stream = ERROR.to_vec();
        // messages after the error and before the sync point are still consumed
        stream.extend_from_slice(b"N\0\0\0\x04S\0\0\0\x04A\0\0\0\x04");
        stream.extend_from_slice(b"Z\0\0\0\x05E");
        stream.extend_from_slice(b"C\0\0\0\x0dUPDATE 3\0");
        stream.extend_from_slice(READY);
        let mut link = answered(&stream);
        let error = link.read(1, 0, Sink::Discard).unwrap_err();
        assert_eq!(error.code(), Some("22012"));
        assert!(!link.broken);
        assert_eq!(link.status, b'E');
        assert_eq!(link.read(1, 0, Sink::Discard).unwrap(), 3);
        assert_eq!(link.status, b'I');
    }

    #[test]
    fn several_responses_are_read_as_one() {
        let mut stream = b"3\0\0\0\x04".to_vec();
        stream.extend_from_slice(READY);
        stream.extend_from_slice(ERROR);
        stream.extend_from_slice(READY);
        let mut link = answered(&stream);
        assert_eq!(
            link.read(2, 0, Sink::Discard).unwrap_err().code(),
            Some("22012")
        );
        assert!(!link.broken);
    }

    #[test]
    fn a_fatal_error_keeps_its_cause_and_ends_the_link() {
        let mut link = answered(ERROR);
        assert_eq!(
            link.read(1, 0, Sink::Discard).unwrap_err().code(),
            Some("22012")
        );
        assert!(link.broken);
        assert!(matches!(link.simple("SELECT 1"), Err(Error::Closed)));
    }

    #[test]
    fn malformed_responses_end_the_link() {
        for response in [
            &b"x\0\0\0\x04"[..],
            b"Z\0\0\0\x05Q",
            b"Z\0\0\0\x04",
            b"Z\0\0\0\x06II",
            b"H\0\0\0\x04",
            b"C\0\0\0\x08SELE",
            b"E\0\0\0\x0aSERROR",
            b"D\0\0\0\x06\0\x01",
            b"Z\0\0\0\x05",
        ] {
            let mut link = answered(response);
            let mut rows = RowSet::new(Arc::from(Vec::new()));
            let result = link.read(1, 0, Sink::Rows(&mut rows));
            assert!(
                matches!(result, Err(Error::Protocol(_) | Error::Io(_))),
                "{response:?}"
            );
            assert!(link.broken, "{response:?}");
        }
    }

    fn field(name: &str, ty: Oid) -> Vec<u8> {
        let mut out = name.as_bytes().to_vec();
        out.push(0);
        out.extend_from_slice(&[0, 0, 64, 0, 0, 1]);
        out.extend_from_slice(&ty.to_be_bytes());
        out.extend_from_slice(&[0, 8, 255, 255, 255, 255, 0, 0]);
        out
    }

    #[test]
    fn row_descriptions_are_checked_field_by_field() {
        let mut body = vec![0, 2];
        body.extend(field("id", oid::INT8));
        body.extend(field("name", oid::TEXT));
        let parsed = columns(&body).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            (parsed[1].name(), parsed[1].type_oid()),
            ("name", oid::TEXT)
        );
        for cut in 0..body.len() {
            assert!(columns(&body[..cut]).is_err(), "truncated at {cut}");
        }
        body.push(0);
        assert!(columns(&body).is_err());
        // a count far beyond the bytes present must not reserve memory for it
        assert!(columns(&[0xff, 0xff]).is_err());
        let mut invalid = vec![0, 1];
        invalid.extend(field("\u{0}", oid::INT8));
        assert!(columns(&invalid).is_err());
    }

    #[test]
    fn parameter_descriptions_are_checked() {
        assert_eq!(
            *parameter_types(&[0, 2, 0, 0, 0, 20, 0, 0, 14, 218]).unwrap(),
            [oid::INT8, oid::JSONB]
        );
        assert!(parameter_types(&[0, 2, 0, 0, 0, 20]).is_err());
        assert!(parameter_types(&[0, 0, 0]).is_err());
        assert!(parameter_types(&[0]).is_err());
    }
}
