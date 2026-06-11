//! Reftable stack reading (ref records only; ref logs are outside M1 scope).
//!
//! Spec: git Documentation/technical/reftable. Key traps encoded here:
//! the varint scheme is the chained `((v+1) << 7)` one (not LEB128), the
//! first block's offsets are file-absolute (they include the 24/28-byte
//! file header), and newer tables in `tables.list` shadow older ones with
//! type-0 records acting as tombstones.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use alt_git_codec::{HashAlgo, ObjectId};
use bstr::BString;

use crate::{Ref, RefError, RefTarget};

/// Materializes the merged ref view of the stack under `git_dir/reftable`.
pub(crate) fn read_stack(git_dir: &Path, algo: HashAlgo) -> Result<Vec<Ref>, RefError> {
    let dir = git_dir.join("reftable");
    let list = fs::read_to_string(dir.join("tables.list"))?;

    // oldest → newest: later inserts win; `None` is a tombstone
    type Value = Option<(RefTarget, Option<ObjectId>)>;
    let mut merged: BTreeMap<BString, Value> = BTreeMap::new();
    for line in list.lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        for record in parse_table(&fs::read(dir.join(name))?, algo)? {
            merged.insert(record.name, record.value);
        }
    }
    Ok(merged
        .into_iter()
        .filter_map(|(name, value)| {
            value.map(|(target, peeled)| Ref {
                name,
                target,
                peeled,
            })
        })
        .collect())
}

struct Record {
    name: BString,
    value: Option<(RefTarget, Option<ObjectId>)>,
}

fn parse_table(data: &[u8], algo: HashAlgo) -> Result<Vec<Record>, RefError> {
    const ERR: fn(&'static str) -> RefError = RefError::Format;
    if data.len() < 24 || &data[..4] != b"REFT" {
        return Err(ERR("not a reftable (bad magic)"));
    }
    let header_size = match data[4] {
        1 => 24,
        2 => 28,
        _ => return Err(ERR("unsupported reftable version")),
    };
    let block_size = u32::from_be_bytes([0, data[5], data[6], data[7]]) as usize;

    let mut out = Vec::new();
    let mut origin = 0usize;
    loop {
        // the first block's content starts after the file header, and its
        // offsets (block_len, restarts) are relative to file position 0
        let header_at = if origin == 0 { header_size } else { origin };
        if header_at + 4 > data.len() || data[header_at] != b'r' {
            break; // end of the ref section ('i'/'o'/'g' block or footer)
        }
        let block_len = u32::from_be_bytes([
            0,
            data[header_at + 1],
            data[header_at + 2],
            data[header_at + 3],
        ]) as usize;
        let block_end = origin + block_len;
        if block_end > data.len() || block_end < header_at + 6 {
            return Err(ERR("reftable block overruns file"));
        }
        let restart_count = u16::from_be_bytes([data[block_end - 2], data[block_end - 1]]) as usize;
        let records_end = block_end
            .checked_sub(2 + 3 * restart_count)
            .ok_or(ERR("reftable restart table overruns block"))?;

        let mut pos = header_at + 4;
        let mut prior: Vec<u8> = Vec::new();
        while pos < records_end {
            let record = parse_record(data, &mut pos, &mut prior, algo)?;
            out.push(record);
        }

        origin = if block_size != 0 {
            if origin == 0 {
                block_size
            } else {
                origin + block_size
            }
        } else {
            block_end
        };
        if origin >= data.len() {
            break;
        }
    }
    Ok(out)
}

fn parse_record(
    data: &[u8],
    pos: &mut usize,
    prior: &mut Vec<u8>,
    algo: HashAlgo,
) -> Result<Record, RefError> {
    const ERR: fn(&'static str) -> RefError = RefError::Format;
    let prefix_len = varint(data, pos)? as usize;
    let word = varint(data, pos)?;
    let suffix_len = (word >> 3) as usize;
    let value_type = (word & 7) as u8;

    if prefix_len > prior.len() {
        return Err(ERR("reftable prefix exceeds prior key"));
    }
    let suffix = data
        .get(*pos..*pos + suffix_len)
        .ok_or(ERR("truncated reftable key suffix"))?;
    *pos += suffix_len;
    prior.truncate(prefix_len);
    prior.extend_from_slice(suffix);
    let name: BString = prior.as_slice().into();

    let _update_index_delta = varint(data, pos)?;

    let raw = algo.raw_len();
    let take_oid = |pos: &mut usize| -> Result<ObjectId, RefError> {
        let bytes = data
            .get(*pos..*pos + raw)
            .ok_or(ERR("truncated reftable oid"))?;
        *pos += raw;
        Ok(ObjectId::from_bytes(algo, bytes).unwrap())
    };
    let value = match value_type {
        0 => None, // tombstone
        1 => Some((RefTarget::Direct(take_oid(pos)?), None)),
        2 => {
            let oid = take_oid(pos)?;
            let peeled = take_oid(pos)?;
            Some((RefTarget::Direct(oid), Some(peeled)))
        }
        3 => {
            let len = varint(data, pos)? as usize;
            let target = data
                .get(*pos..*pos + len)
                .ok_or(ERR("truncated reftable symref target"))?;
            *pos += len;
            Some((RefTarget::Symbolic(target.into()), None))
        }
        _ => return Err(ERR("reserved reftable value type")),
    };
    Ok(Record { name, value })
}

/// Reftable's chained varint: `v = ((v + 1) << 7) | (b & 0x7f)` per
/// continuation byte — the same family as pack ofs-delta distances,
/// not LEB128.
fn varint(d: &[u8], pos: &mut usize) -> Result<u64, RefError> {
    let mut b = *d
        .get(*pos)
        .ok_or(RefError::Format("truncated reftable varint"))?;
    *pos += 1;
    let mut v = u64::from(b & 0x7f);
    while b & 0x80 != 0 {
        b = *d
            .get(*pos)
            .ok_or(RefError::Format("truncated reftable varint"))?;
        *pos += 1;
        v = ((v + 1) << 7) | u64::from(b & 0x7f);
    }
    Ok(v)
}
