//! Manifest nodes: the tree of chunk ids a multi-chunk blob is stored as.

use crate::{ChunkId, StoreError};

const NODE_MAGIC: [u8; 4] = *b"ALTM";
const NODE_VERSION: u8 = 1;
/// Node header: magic + version + level u8 + count u32.
const NODE_HEADER_LEN: usize = 10;
/// Entry: chunk id 32 + span u64.
const NODE_ENTRY_LEN: usize = 40;

#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeEntry {
    pub id: ChunkId,
    /// Bytes covered: chunk length at level 0, subtree total above.
    pub span: u64,
}

pub(crate) fn encode_node(level: u8, entries: &[NodeEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(NODE_HEADER_LEN + NODE_ENTRY_LEN * entries.len());
    out.extend_from_slice(&NODE_MAGIC);
    out.push(NODE_VERSION);
    out.push(level);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        out.extend_from_slice(&entry.id.0);
        out.extend_from_slice(&entry.span.to_le_bytes());
    }
    out
}

pub(crate) fn parse_node(data: &[u8]) -> Result<(u8, Vec<NodeEntry>), StoreError> {
    if data.len() < NODE_HEADER_LEN || data[..4] != NODE_MAGIC {
        return Err(StoreError::Format("bad manifest node header"));
    }
    if data[4] != NODE_VERSION {
        return Err(StoreError::Format("unsupported manifest node version"));
    }
    let level = data[5];
    let count = u32::from_le_bytes(data[6..10].try_into().unwrap()) as usize;
    if count
        .checked_mul(NODE_ENTRY_LEN)
        .and_then(|n| n.checked_add(NODE_HEADER_LEN))
        != Some(data.len())
    {
        return Err(StoreError::Format("manifest node length mismatch"));
    }
    let mut entries = Vec::with_capacity(count);
    for raw in data[NODE_HEADER_LEN..].as_chunks::<NODE_ENTRY_LEN>().0 {
        let mut id = [0u8; 32];
        id.copy_from_slice(&raw[..32]);
        entries.push(NodeEntry {
            id: ChunkId(id),
            span: u64::from_le_bytes(raw[32..].try_into().unwrap()),
        });
    }
    Ok((level, entries))
}
