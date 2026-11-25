//! The bytes of a log file as a handle reads them.

use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::scan::parse_body;
use crate::v2::frame::View;
use crate::{Framed, Op, OpLogError};

/// Bytes of the log file as an open reads them, counted.
pub(crate) struct Source<'a> {
    pub file: &'a File,
    /// how many bytes were read, and in how many reads
    pub read: &'a Cell<(u64, u64)>,
}

impl Source<'_> {
    /// The `len` bytes at `at`, read into memory nothing was written to
    /// first: a handle of this format writes at offsets and keeps no place
    /// in the file.
    pub fn at(&self, at: u64, len: usize) -> std::io::Result<Vec<u8>> {
        let (bytes, reads) = self.read.get();
        self.read.set((bytes + len as u64, reads + 1));
        let mut data = Vec::with_capacity(len);
        let mut file = self.file;
        file.seek(SeekFrom::Start(at))?;
        file.take(len as u64).read_to_end(&mut data)?;
        match data.len() == len {
            true => Ok(data),
            false => Err(std::io::ErrorKind::UnexpectedEof.into()),
        }
    }

    /// The stretches of the file between byte `from` and byte `to` that
    /// hold data, in their order, as far as the file system tells them
    /// from holes: what lies between them reads as zeros, and nobody has
    /// to read it to know that no block of the log begins there.
    pub fn stretches(&self, from: u64, to: u64) -> std::io::Result<Vec<(u64, u64)>> {
        let mut held = Vec::new();
        let mut at = from;
        while at < to
            && let Some((start, end)) = alt_os::holes::data_from(self.file, at)?
            && start < to
            && end > at
        {
            held.push((start, end.min(to)));
            at = end;
        }
        Ok(held)
    }
}

/// The op an op frame at `at..end` holds.
pub(crate) fn op_of(at: u64, end: u64, view: &View) -> Result<Op, OpLogError> {
    let Some(id) = view.op else {
        return Err(OpLogError::Format("an op frame without an id"));
    };
    let mut op = parse_body(view.body, id)?;
    op.frame = Some(Framed {
        at,
        end,
        seq: view.head.seq,
        flags: view.head.flags,
        workspace: view.head.workspace,
        linked: view.ext.linked.is_some(),
        inverse_of: view.ext.inverse_of,
        request: view.ext.request,
        staged: view.ext.index_after.is_some(),
        retired: view.ext.retired,
    });
    Ok(op)
}
