//! Producing packs from stored segments: whole segments verbatim, or a chosen object set
//! whose compressed entries are copied without recompression.

use crate::{
    Error, Kind, Oid, SegmentSet,
    format::Delta,
    object::{self, Parsed},
    scan::{self, Form},
};
use std::{
    collections::{BinaryHeap, HashMap, HashSet},
    io::Write,
};

struct Hashing<'a, W: Write> {
    inner: &'a mut W,
    hasher: alt_hash::Sha1,
}

impl<W: Write> Hashing<'_, W> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.hasher.update(bytes);
        self.inner.write_all(bytes)?;
        Ok(())
    }

    fn finish(self) -> Result<(), Error> {
        let trailer = self.hasher.finalize();
        self.inner.write_all(&trailer)?;
        Ok(())
    }
}

fn pack_header(count: u64) -> Result<[u8; 12], Error> {
    let count = u32::try_from(count).map_err(|_| Error::Limit("objects in one pack"))?;
    let mut header = *b"PACK\0\0\0\x02\0\0\0\0";
    header[8..].copy_from_slice(&count.to_be_bytes());
    Ok(header)
}

fn entry_header(code: u8, mut size: u64) -> Vec<u8> {
    let mut out = vec![code << 4 | (size & 15) as u8];
    size >>= 4;
    while size != 0 {
        *out.last_mut().unwrap() |= 0x80;
        out.push((size & 0x7f) as u8);
        size >>= 7;
    }
    out
}

/// a self-contained pack of whole objects, for content that did not arrive as a pack
pub fn build_pack<'a>(
    objects: impl ExactSizeIterator<Item = (Kind, &'a [u8])>,
) -> Result<Vec<u8>, Error> {
    let mut pack = pack_header(objects.len() as u64)?.to_vec();
    for (kind, data) in objects {
        pack.extend_from_slice(&entry_header(kind as u8, data.len() as u64));
        let mut encoder = flate2::write::ZlibEncoder::new(pack, flate2::Compression::fast());
        encoder.write_all(data)?;
        pack = encoder.finish()?;
    }
    let trailer = alt_hash::sha1(&pack);
    pack.extend_from_slice(&trailer);
    Ok(pack)
}

const WANT: u8 = 1;
const HAVE: u8 = 2;

/// adds `bit` to a commit's flags, queueing it on first sight; `open` counts queued commits
/// that are wanted and not known to be held
fn mark(
    flags: &mut HashMap<Oid, u8>,
    heap: &mut BinaryHeap<(u32, Oid)>,
    open: &mut usize,
    oid: Oid,
    bit: u8,
    generation: u32,
) {
    let entry = flags.entry(oid).or_insert_with(|| {
        heap.push((generation, oid));
        0
    });
    let before = *entry;
    *entry |= bit;
    match (before, *entry) {
        (0, WANT) => *open += 1,
        (WANT, both) if both & HAVE != 0 => *open -= 1,
        _ => {}
    }
}

/// the objects a client asked for and does not have
pub struct Selection {
    objects: Vec<Oid>,
    /// objects the client is known to hold; usable as delta bases of a thin pack
    known: HashSet<Oid>,
}

impl Selection {
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn objects(&self) -> &[Oid] {
        &self.objects
    }
}

impl SegmentSet {
    /// every segment can be sent as stored: no duplicate objects within or across segments
    pub fn verbatim(&self) -> bool {
        self.segments().iter().all(|segment| segment.flags() == 0)
    }

    /// all objects of all segments as one pack; entries are copied as stored
    pub fn write_all(&self, out: &mut impl Write) -> Result<(), Error> {
        if let [only] = self.segments() {
            out.write_all(only.pack())?;
            return Ok(());
        }
        let mut out = Hashing {
            inner: out,
            hasher: alt_hash::Sha1::new(),
        };
        out.put(&pack_header(self.objects())?)?;
        for segment in self.segments() {
            let pack = segment.pack();
            out.put(&pack[12..pack.len() - 20])?;
        }
        out.finish()
    }

    fn tree_objects(
        &self,
        tree: &Oid,
        seen: &mut HashSet<Oid>,
        skip: &HashSet<Oid>,
        out: Option<&mut Vec<Oid>>,
    ) -> Result<(), Error> {
        let mut out = out;
        let mut pending = vec![*tree];
        while let Some(oid) = pending.pop() {
            if skip.contains(&oid) || !seen.insert(oid) {
                continue;
            }
            if let Some(out) = out.as_deref_mut() {
                out.push(oid);
            }
            let (kind, data) = self.read(&oid)?;
            let Parsed::Tree(entries) = object::parse(kind, &data)? else {
                return Err(Error::Corrupt("tree expected"));
            };
            for (child, kind) in entries {
                match kind {
                    Kind::Tree => pending.push(child),
                    _ if skip.contains(&child) || !seen.insert(child) => {}
                    _ => {
                        if let Some(out) = out.as_deref_mut() {
                            out.push(child);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// objects reachable from `wants` and not from `haves`; unknown haves are ignored
    pub fn select(&self, wants: &[Oid], haves: &[Oid]) -> Result<Selection, Error> {
        let mut objects = Vec::new();
        let mut seen = HashSet::new();
        let mut flags: HashMap<Oid, u8> = HashMap::new();
        let mut heap = BinaryHeap::new();
        let mut open = 0usize;
        let mut trees = Vec::new();
        for want in wants {
            // peel tags down to what they name
            let mut oid = *want;
            loop {
                match self.kind(&oid)?.ok_or(Error::NotFound(oid))? {
                    Kind::Commit => {
                        let generation = self
                            .commit(&oid)
                            .ok_or(Error::Corrupt("commit graph"))?
                            .generation;
                        mark(&mut flags, &mut heap, &mut open, oid, WANT, generation);
                        break;
                    }
                    Kind::Tag => {
                        if seen.insert(oid) {
                            objects.push(oid);
                        }
                        let (kind, data) = self.read(&oid)?;
                        let Parsed::Tag { object, .. } = object::parse(kind, &data)? else {
                            unreachable!()
                        };
                        oid = object;
                    }
                    Kind::Tree => {
                        trees.push(oid);
                        break;
                    }
                    Kind::Blob => {
                        if seen.insert(oid) {
                            objects.push(oid);
                        }
                        break;
                    }
                }
            }
        }
        for have in haves {
            if let Some(commit) = self.commit(have) {
                mark(
                    &mut flags,
                    &mut heap,
                    &mut open,
                    *have,
                    HAVE,
                    commit.generation,
                );
            }
        }
        // children always carry a higher generation than their parents, so a commit's flags
        // are final when it leaves the heap
        let mut commits = Vec::new();
        let mut boundary = Vec::new();
        while open > 0 {
            let Some((_, oid)) = heap.pop() else { break };
            let state = flags[&oid];
            let commit = self.commit(&oid).ok_or(Error::Corrupt("commit graph"))?;
            if state & HAVE != 0 {
                boundary.push(commit.tree);
            } else {
                open -= 1;
                commits.push((oid, commit.tree));
            }
            for parent in &commit.parents {
                let generation = self
                    .commit(parent)
                    .ok_or(Error::Corrupt("commit graph"))?
                    .generation;
                mark(
                    &mut flags,
                    &mut heap,
                    &mut open,
                    *parent,
                    if state & HAVE != 0 { HAVE } else { WANT },
                    generation,
                );
            }
        }
        for (_, oid) in heap {
            if flags[&oid] & HAVE != 0
                && let Some(commit) = self.commit(&oid)
            {
                boundary.push(commit.tree);
            }
        }
        let mut known = HashSet::new();
        let nothing = HashSet::new();
        for tree in &boundary {
            self.tree_objects(tree, &mut known, &nothing, None)?;
        }
        for (oid, tree) in &commits {
            if seen.insert(*oid) {
                objects.push(*oid);
            }
            self.tree_objects(tree, &mut seen, &known, Some(&mut objects))?;
        }
        for tree in &trees {
            self.tree_objects(tree, &mut seen, &known, Some(&mut objects))?;
        }
        Ok(Selection { objects, known })
    }

    /// writes `selection` as one pack; with `thin`, deltas may rest on objects the client holds
    pub fn write_selected(
        &self,
        selection: &Selection,
        thin: bool,
        out: &mut impl Write,
    ) -> Result<(), Error> {
        let chosen: HashSet<&Oid> = selection.objects.iter().collect();
        let mut out = Hashing {
            inner: out,
            hasher: alt_hash::Sha1::new(),
        };
        out.put(&pack_header(selection.objects.len() as u64)?)?;
        for oid in &selection.objects {
            let (segment, index) = self.locate(oid).ok_or(Error::NotFound(*oid))?;
            let record = segment.record(index)?;
            let entry = segment.entry(&record)?;
            if record.delta == Delta::Plain {
                out.put(entry)?;
                continue;
            }
            let (form, size, head) = scan::header(entry, 0)?;
            let base = match (record.delta, form) {
                (Delta::Inside, _) => segment.record(record.base as usize)?.oid,
                (_, Form::Reference(base)) => base,
                _ => return Err(Error::Corrupt("record and entry disagree")),
            };
            if chosen.contains(&base) || (thin && selection.known.contains(&base)) {
                out.put(&entry_header(7, size))?;
                out.put(&base)?;
                out.put(&entry[head..])?;
                continue;
            }
            let (kind, data) = self.read(oid)?;
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&data)?;
            out.put(&entry_header(kind as u8, data.len() as u64))?;
            out.put(&encoder.finish()?)?;
        }
        out.finish()
    }
}
