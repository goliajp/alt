//! What a frame carries beside its body: items of
//! `[tag u8][length u8][content]` between its header and its body. An item
//! of a tag a reader does not know is passed over.

/// How far the four append-only files of the object store are synced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OdbBounds {
    pub lens: [u64; 4],
    pub generation: u32,
}

/// Bytes that were cut off the end of the log before the frame that says
/// so, and how many ops among them read: ops of the log that a writer
/// looked at and cut, or what read past where an append was left
/// unfinished, which was no log. The frame also tells under which
/// generation the frames before it were written, back to the frame that
/// says the same of an earlier cut: a frame from before a cut reads under
/// its own generation, and a reader finds that from the last cut back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    pub bytes: u64,
    pub ops: u32,
    /// the bytes held frames of the log, up to which a sync may have
    /// been made
    pub of_the_log: bool,
    /// where the frame of the cut before this one starts, or zero
    pub after: u64,
    /// the generation of the frames between that one and this
    pub generation: u32,
}

/// The part an op has in a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Begin,
    /// within the step begun by the frame at this offset
    Within(u64),
    Close,
}

/// What a frame carries beside its body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ext {
    pub odb: Option<OdbBounds>,
    /// the frame of the op this one undoes
    pub inverse_of: Option<u64>,
    pub request: Option<[u8; 16]>,
    /// on a ref transaction: the frame of the one before it, or zero
    pub linked: Option<u64>,
    pub cut: Option<Cut>,
    /// the checksum that closes the index once the op is done
    pub index_after: Option<[u8; 20]>,
    pub step: Option<Step>,
    /// the workspace is gone with this op
    pub retired: bool,
}

fn item(out: &mut Vec<u8>, tag: u8, content: &[u8]) {
    out.push(tag);
    out.push(content.len() as u8);
    out.extend_from_slice(content);
}

pub(super) fn word(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

impl Ext {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(odb) = &self.odb {
            let mut content = [0u8; 36];
            for (index, len) in odb.lens.iter().enumerate() {
                content[index * 8..index * 8 + 8].copy_from_slice(&len.to_le_bytes());
            }
            content[32..].copy_from_slice(&odb.generation.to_le_bytes());
            item(&mut out, 1, &content);
        }
        if let Some(at) = self.inverse_of {
            item(&mut out, 2, &at.to_le_bytes());
        }
        if let Some(request) = &self.request {
            item(&mut out, 3, request);
        }
        if let Some(at) = self.linked {
            item(&mut out, 4, &at.to_le_bytes());
        }
        if let Some(cut) = &self.cut {
            let mut content = [0u8; 24];
            content[..8].copy_from_slice(&cut.bytes.to_le_bytes());
            let ops = cut.ops | (cut.of_the_log as u32) << 31;
            content[8..12].copy_from_slice(&ops.to_le_bytes());
            content[12..20].copy_from_slice(&cut.after.to_le_bytes());
            content[20..].copy_from_slice(&cut.generation.to_le_bytes());
            item(&mut out, 5, &content);
        }
        if let Some(sum) = &self.index_after {
            item(&mut out, 6, sum);
        }
        if let Some(step) = self.step {
            let (what, begin) = match step {
                Step::Begin => (0, 0),
                Step::Within(begin) => (1, begin),
                Step::Close => (2, 0),
            };
            let mut content = [what; 9];
            content[1..].copy_from_slice(&begin.to_le_bytes());
            item(&mut out, 7, &content);
        }
        if self.retired {
            item(&mut out, 8, &[]);
        }
        out
    }

    /// `None` for bytes that are no run of items, or a known item of a
    /// length it never has. An item of an unknown tag is passed over.
    pub fn decode(mut bytes: &[u8]) -> Option<Self> {
        let mut ext = Ext::default();
        while let [tag, len, rest @ ..] = bytes {
            let content = rest.get(..*len as usize)?;
            bytes = &rest[*len as usize..];
            match (*tag, content.len()) {
                (1, 36) => {
                    let lens = std::array::from_fn(|index| word(content, index * 8));
                    let generation = u32::from_le_bytes(content[32..].try_into().unwrap());
                    ext.odb = Some(OdbBounds { lens, generation });
                }
                (2, 8) => ext.inverse_of = Some(word(content, 0)),
                (3, 16) => ext.request = Some(content.try_into().unwrap()),
                (4, 8) => ext.linked = Some(word(content, 0)),
                (5, 24) => {
                    let part =
                        |at: usize| u32::from_le_bytes(content[at..at + 4].try_into().unwrap());
                    ext.cut = Some(Cut {
                        bytes: word(content, 0),
                        ops: part(8) & !(1 << 31),
                        of_the_log: part(8) >> 31 != 0,
                        after: word(content, 12),
                        generation: part(20),
                    });
                }
                (6, 20) => ext.index_after = Some(content.try_into().unwrap()),
                (7, 9) => {
                    ext.step = Some(match content[0] {
                        0 => Step::Begin,
                        1 => Step::Within(word(content, 1)),
                        2 => Step::Close,
                        _ => return None,
                    })
                }
                (8, 0) => ext.retired = true,
                (1..=8, _) => return None,
                _ => {}
            }
        }
        bytes.is_empty().then_some(ext)
    }
}
