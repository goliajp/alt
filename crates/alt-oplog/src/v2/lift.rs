//! Reading a frame out of the blocks it lies in.

use std::borrow::Cow;

use crate::v2::block::{BLOCK_HEADER_LEN, BlockHeader, Layout};

impl Layout {
    /// The first `len` bytes of a frame that starts at `at`, fewer than a
    /// block holds, read out of `data` past the block header they may run
    /// across. What that header says of the frame is not looked at: it
    /// tells where the whole frame ends, which these bytes are read to
    /// learn.
    pub fn lift_start<'a>(
        &self,
        data: &'a [u8],
        base: u64,
        at: u64,
        len: usize,
    ) -> Option<Cow<'a, [u8]>> {
        let from = usize::try_from(at.checked_sub(base)?).ok()?;
        let room = self.room(at).min(len as u64) as usize;
        let first = data.get(from..from.checked_add(room)?)?;
        if room == len {
            return Some(Cow::Borrowed(first));
        }
        let rest = data.get(from + room + BLOCK_HEADER_LEN..)?;
        Some(Cow::Owned([first, rest.get(..len - room)?].concat()))
    }

    /// The `len` bytes of a frame that starts at `at`, read out of `data`
    /// (which begins at byte `base` of the file) with the block headers it
    /// runs across left out, and where it ends in the file. `None` where
    /// `data` does not hold all of it or one of those headers does not
    /// read as a header inside this frame.
    pub fn lift<'a>(
        &self,
        data: &'a [u8],
        base: u64,
        at: u64,
        len: u64,
    ) -> Option<(Cow<'a, [u8]>, u64)> {
        self.lift_across(data, base, at, len, &mut |_| {})
    }

    /// [`Self::lift`], with each block header the frame runs across
    /// given to `crossed`.
    pub fn lift_across<'a>(
        &self,
        data: &'a [u8],
        base: u64,
        at: u64,
        len: u64,
        crossed: &mut dyn FnMut(&BlockHeader),
    ) -> Option<(Cow<'a, [u8]>, u64)> {
        let end = self.end_of(at, len)?;
        let from = usize::try_from(at.checked_sub(base)?).ok()?;
        let held = data.get(from..usize::try_from(end - base).ok()?)?;
        let room = self.room(at);
        if len <= room {
            return Some((Cow::Borrowed(held), end));
        }
        let (first, mut rest) = held.split_at(room as usize);
        let mut frame = Vec::with_capacity(len as usize);
        frame.extend_from_slice(first);
        let mut block = at + room;
        while !rest.is_empty() {
            let header = self.decode_header(rest, block)?;
            let left = len as usize - frame.len();
            let each = self.block as usize - BLOCK_HEADER_LEN;
            let expected = if left >= each {
                0
            } else {
                BLOCK_HEADER_LEN + left
            };
            if header.first as usize != expected {
                return None;
            }
            crossed(&header);
            let part = left.min(each);
            frame.extend_from_slice(&rest[BLOCK_HEADER_LEN..BLOCK_HEADER_LEN + part]);
            rest = &rest[BLOCK_HEADER_LEN + part..];
            block += self.block;
        }
        Some((Cow::Owned(frame), end))
    }
}
