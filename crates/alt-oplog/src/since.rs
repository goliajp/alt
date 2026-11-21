//! Reading the chain from somewhere other than where this handle began: all
//! of it, or what follows an op a caller has kept an anchor of.

use std::borrow::Cow;

use crate::read::read_at;
use crate::scan::{CHECKSUM_LEN, HEADER_LEN, check_header, replay_records};
use crate::{Anchor, Op, OpLog, OpLogError, ROOT};

impl OpLog {
    /// All ops in chain order — the replay basis. On a log opened at an
    /// anchor the first call reads and verifies the part before it.
    pub fn ops(&self) -> Result<&[Op], OpLogError> {
        let Some(base) = self.base else {
            return Ok(&self.ops);
        };
        if let Some(full) = self.full.get() {
            return Ok(full);
        }
        // the records before the anchor never change; no lock needed
        let data = read_at(&self.file, 0, base.end as usize)?;
        check_header(&data)?;
        let mut all = Vec::new();
        let replayed = replay_records(&data, HEADER_LEN.min(data.len()), ROOT, &mut all)?;
        if replayed.valid as u64 != base.end
            || all.len() as u64 != base.count
            || all.last().map(|op| op.id) != Some(base.op)
        {
            return Err(OpLogError::Format("oplog does not lead to its anchor"));
        }
        all.extend_from_slice(&self.ops);
        Ok(self.full.get_or_init(|| all))
    }

    /// The ops after the one `anchor` names, in chain order, when the
    /// chain this handle holds passes through that op where the anchor
    /// says; `None` when it does not (the log was cut back and grew
    /// differently, or the anchor is of another log). Only the records
    /// between the anchor and where this handle began are read.
    pub fn since(&self, anchor: &Anchor) -> Result<Option<Cow<'_, [Op]>>, OpLogError> {
        let (began, began_at) = match self.base {
            Some(base) => (base.count, Some(base)),
            None => (0, None),
        };
        if anchor.count >= began {
            // an op's id names its whole history, so the id at that count is enough
            let held = (anchor.count - began) as usize;
            let named = match held.checked_sub(1) {
                Some(last) => self.ops.get(last).map(|op| op.id),
                None => began_at.map(|base| base.op),
            };
            return Ok((named == Some(anchor.op)).then(|| Cow::Borrowed(&self.ops[held..])));
        }
        let Some(base) = began_at else {
            return Ok(None);
        };
        let closes = CHECKSUM_LEN as u64;
        if anchor.end > base.start
            || anchor.end < closes
            || read_at(&self.file, anchor.end - closes, CHECKSUM_LEN)?
                != anchor.op.0[..CHECKSUM_LEN]
        {
            return Ok(None);
        }
        // the first record read names the anchor's op as its parent, in full
        let data = read_at(&self.file, anchor.end, (base.end - anchor.end) as usize)?;
        let mut between = Vec::new();
        let Ok(replayed) = replay_records(&data, 0, anchor.op, &mut between) else {
            return Ok(None);
        };
        if replayed.valid != data.len()
            || anchor.count + between.len() as u64 != base.count
            || between.last().map(|op| op.id) != Some(base.op)
        {
            return Ok(None);
        }
        between.extend_from_slice(&self.ops);
        Ok(Some(Cow::Owned(between)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_of(dir: &std::path::Path, actor: &str, ops: usize) -> Vec<Anchor> {
        let mut log = OpLog::open(dir).unwrap();
        let mut anchors = Vec::new();
        for n in 0..ops {
            log.append(actor, n as u64, format!("op {n}").as_bytes())
                .unwrap();
            anchors.push(log.anchor().unwrap());
        }
        anchors
    }

    fn payloads(ops: &[Op]) -> Vec<String> {
        let text = |op: &Op| String::from_utf8(op.payload.clone()).unwrap();
        ops.iter().map(text).collect()
    }

    #[test]
    fn the_ops_after_an_anchor_are_the_same_wherever_the_handle_began() {
        let dir = alt_os::temp::tempdir().unwrap();
        let anchors = log_of(dir.path(), "a", 6);
        let whole = OpLog::open(dir.path()).unwrap();
        for began in [None, Some(anchors[1]), Some(anchors[3]), Some(anchors[5])] {
            let log = OpLog::open_at(dir.path(), began).unwrap();
            assert_eq!(log.anchored(), began.is_some());
            for (n, anchor) in anchors.iter().enumerate() {
                let after = log.since(anchor).unwrap().unwrap();
                assert_eq!(payloads(&after), payloads(&whole.ops().unwrap()[n + 1..]));
            }
        }
    }

    #[test]
    fn an_anchor_the_chain_does_not_pass_through_gives_nothing() {
        let dir = alt_os::temp::tempdir().unwrap();
        let anchors = log_of(dir.path(), "a", 4);
        let other = alt_os::temp::tempdir().unwrap();
        let foreign = log_of(other.path(), "b", 2);
        for began in [None, Some(anchors[2])] {
            let log = OpLog::open_at(dir.path(), began).unwrap();
            // another log's op, the right place under another id, a count past the end
            assert!(log.since(&foreign[1]).unwrap().is_none());
            let renamed = Anchor {
                op: anchors[2].op,
                ..anchors[0]
            };
            assert!(log.since(&renamed).unwrap().is_none());
            let past = Anchor {
                count: 9,
                ..anchors[3]
            };
            assert!(log.since(&past).unwrap().is_none());
        }
        // a place before where the handle began that no record ends at
        let log = OpLog::open_at(dir.path(), Some(anchors[2])).unwrap();
        let shifted = Anchor {
            end: anchors[0].end - 1,
            ..anchors[0]
        };
        assert!(log.since(&shifted).unwrap().is_none());
    }
}
