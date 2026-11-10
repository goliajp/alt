//! Version-2 pack/idx writing, with optional bounded in-pack REF deltas.
//!
//! The pack streams through a hashing writer (the trailer is the running
//! digest) with a per-entry crc32 for the idx; both files are written to
//! temp names and renamed to `pack-<trailer-hex>.{pack,idx}` at finish.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_hash::{Sha1, Sha256};

use crate::{DeltaOptions, PackError, PlainRepresentation, delta_window::Window};

enum Hasher {
    Sha1(Sha1),
    Sha256(Sha256),
}

impl Hasher {
    fn new(algo: HashAlgo) -> Self {
        match algo {
            HashAlgo::Sha1 => Self::Sha1(Sha1::new()),
            HashAlgo::Sha256 => Self::Sha256(Sha256::new()),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha1(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
        }
    }

    fn finalize(self) -> Vec<u8> {
        match self {
            Self::Sha1(h) => h.finalize().to_vec(),
            Self::Sha256(h) => h.finalize().to_vec(),
        }
    }
}

/// Tee writer: everything written updates the pack digest (and, while an
/// entry is open, its crc32).
struct HashWriter<W: Write> {
    inner: W,
    hasher: Hasher,
    crc: Option<alt_hash::Crc32>,
    written: u64,
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        if let Some(crc) = &mut self.crc {
            crc.update(&buf[..n]);
        }
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The files a finished write produced.
#[derive(Debug)]
pub struct WrittenPack {
    pub pack_path: PathBuf,
    pub idx_path: PathBuf,
    /// The pack trailer digest — also the file name stem.
    pub trailer: Vec<u8>,
    pub objects: u32,
}

/// Streams a version-2 pack: declare the object count up front, `add`
/// every object once, `finish` to seal trailer + idx.
pub struct PackWriter {
    dir: PathBuf,
    algo: HashAlgo,
    out: HashWriter<BufWriter<File>>,
    tmp_pack: PathBuf,
    entries: Vec<(ObjectId, u64, u32)>,
    declared: u32,
    deltas: Option<Window>,
}

impl PackWriter {
    pub fn create(pack_dir: &Path, algo: HashAlgo, count: u32) -> Result<Self, PackError> {
        fs::create_dir_all(pack_dir)?;
        let tmp_pack = pack_dir.join("tmp_pack_writing");
        let file = File::create(&tmp_pack)?;
        let mut out = HashWriter {
            inner: BufWriter::new(file),
            hasher: Hasher::new(algo),
            crc: None,
            written: 0,
        };
        out.write_all(b"PACK")?;
        out.write_all(&2u32.to_be_bytes())?;
        out.write_all(&count.to_be_bytes())?;
        Ok(Self {
            dir: pack_dir.to_owned(),
            algo,
            out,
            tmp_pack,
            entries: Vec::with_capacity(count as usize),
            declared: count,
            deltas: None,
        })
    }

    /// Enables bounded delta selection against previously emitted objects.
    /// Ordinary `create` retains its plain-entry behavior. REF deltas require
    /// no ofs-delta negotiation and the resulting pack remains self-contained.
    pub fn create_with_deltas(
        pack_dir: &Path,
        algo: HashAlgo,
        count: u32,
        options: DeltaOptions,
    ) -> Result<Self, PackError> {
        let window = Window::new(options)?;
        let mut writer = Self::create(pack_dir, algo, count)?;
        writer.deltas = Some(window);
        Ok(writer)
    }

    /// Writes one object. The caller vouches that `oid` is the canonical
    /// object's id. Delta representation never changes its resolved kind/id.
    pub fn add(&mut self, oid: ObjectId, kind: ObjectKind, data: &[u8]) -> Result<(), PackError> {
        let choice = self
            .deltas
            .as_ref()
            .and_then(|window| window.choose(oid, kind, data));
        let type_id = if choice.is_some() {
            7
        } else {
            match kind {
                ObjectKind::Commit => 1,
                ObjectKind::Tree => 2,
                ObjectKind::Blob => 3,
                ObjectKind::Tag => 4,
            }
        };
        let payload = choice
            .as_ref()
            .map(|choice| choice.delta.as_slice())
            .unwrap_or(data);
        self.entry(
            oid,
            type_id,
            choice.as_ref().map(|choice| choice.base),
            payload,
        )?;
        if let Some(window) = &mut self.deltas {
            window.remember(
                oid,
                kind,
                data,
                choice.as_ref().map_or(0, |choice| choice.depth),
            );
        }
        Ok(())
    }

    /// Reuse a plain representation only after comparing its complete decoded
    /// bytes with this input. A mismatching, truncated or damaged representation
    /// falls back to ordinary bounded encoding before any entry is written.
    /// Valid reuse keeps the supplied representation rather than seeking a new
    /// delta; its raw bytes remain available as a base for subsequent entries.
    /// Returns whether the supplied representation was reused.
    pub fn add_reusing(
        &mut self,
        oid: ObjectId,
        kind: ObjectKind,
        data: &[u8],
        representation: &PlainRepresentation,
    ) -> Result<bool, PackError> {
        if !representation.matches(oid, kind, data) {
            self.add(oid, kind, data)?;
            return Ok(false);
        }
        let type_id = match kind {
            ObjectKind::Commit => 1,
            ObjectKind::Tree => 2,
            ObjectKind::Blob => 3,
            ObjectKind::Tag => 4,
        };
        let offset = self.start_entry(type_id, None, data.len())?;
        self.out.write_all(representation.encoded())?;
        self.finish_entry(oid, offset);
        if let Some(window) = &mut self.deltas {
            window.remember(oid, kind, data, 0);
        }
        Ok(true)
    }

    fn entry(
        &mut self,
        oid: ObjectId,
        type_id: u8,
        base: Option<ObjectId>,
        data: &[u8],
    ) -> Result<(), PackError> {
        let offset = self.start_entry(type_id, base, data.len())?;
        self.out
            .write_all(&alt_deflate::compress(data, 6, alt_deflate::Format::Zlib))?;
        self.finish_entry(oid, offset);
        Ok(())
    }

    fn start_entry(
        &mut self,
        type_id: u8,
        base: Option<ObjectId>,
        bytes: usize,
    ) -> Result<u64, PackError> {
        let offset = self.out.written;
        self.out.crc = Some(alt_hash::Crc32::new());

        // entry header: 3-bit type + size varint (low 4 bits first)
        let mut header = Vec::with_capacity(10);
        let mut size = bytes as u64;
        let mut byte = (type_id << 4) | (size & 0xf) as u8;
        size >>= 4;
        while size > 0 {
            header.push(byte | 0x80);
            byte = (size & 0x7f) as u8;
            size >>= 7;
        }
        header.push(byte);
        self.out.write_all(&header)?;
        if let Some(base) = base {
            self.out.write_all(base.as_bytes())?;
        }

        Ok(offset)
    }

    fn finish_entry(&mut self, oid: ObjectId, offset: u64) {
        let crc = self.out.crc.take().expect("entry crc active").finalize();
        self.entries.push((oid, offset, crc));
    }

    /// Seals the pack (trailer digest), writes the idx, and renames both
    /// to their content-derived names.
    pub fn finish(mut self) -> Result<WrittenPack, PackError> {
        if self.entries.len() as u32 != self.declared {
            return Err(PackError::Format("declared object count not met"));
        }
        // the trailer is the digest of everything before it and is not
        // itself hashed: write it past the tee
        let trailer = std::mem::replace(&mut self.out.hasher, Hasher::new(self.algo)).finalize();
        self.out.inner.write_all(&trailer)?;
        self.out.inner.flush()?;
        self.out.inner.get_ref().sync_all()?;

        let hex: String = trailer.iter().map(|b| format!("{b:02x}")).collect();
        let pack_path = self.dir.join(format!("pack-{hex}.pack"));
        let idx_path = self.dir.join(format!("pack-{hex}.idx"));

        let idx_bytes = build_idx(self.algo, &mut self.entries, &trailer);
        let tmp_idx = self.dir.join("tmp_idx_writing");
        let mut idx_file = File::create(&tmp_idx)?;
        idx_file.write_all(&idx_bytes)?;
        idx_file.sync_all()?;

        // pack first, then idx: a reader treats the idx as the entry point
        fs::rename(&self.tmp_pack, &pack_path)?;
        fs::rename(&tmp_idx, &idx_path)?;
        Ok(WrittenPack {
            pack_path,
            idx_path,
            trailer,
            objects: self.declared,
        })
    }
}

/// Builds a v2 idx: fan-out, sorted oids, crc32s, offsets (large table
/// for ≥ 2^31), pack checksum, idx checksum.
fn build_idx(algo: HashAlgo, entries: &mut [(ObjectId, u64, u32)], trailer: &[u8]) -> Vec<u8> {
    entries.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut out = Vec::with_capacity(8 + 256 * 4 + entries.len() * (algo.raw_len() + 8) + 64);
    out.extend_from_slice(&[0xff, b't', b'O', b'c']);
    out.extend_from_slice(&2u32.to_be_bytes());

    let mut fanout = [0u32; 256];
    for (oid, _, _) in entries.iter() {
        fanout[oid.as_bytes()[0] as usize] += 1;
    }
    let mut cumulative = 0u32;
    for count in fanout {
        cumulative += count;
        out.extend_from_slice(&cumulative.to_be_bytes());
    }
    for (oid, _, _) in entries.iter() {
        out.extend_from_slice(oid.as_bytes());
    }
    for (_, _, crc) in entries.iter() {
        out.extend_from_slice(&crc.to_be_bytes());
    }
    let mut large: Vec<u64> = Vec::new();
    for (_, offset, _) in entries.iter() {
        if *offset < (1 << 31) {
            out.extend_from_slice(&(*offset as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&((1u32 << 31) | large.len() as u32).to_be_bytes());
            large.push(*offset);
        }
    }
    for offset in large {
        out.extend_from_slice(&offset.to_be_bytes());
    }
    out.extend_from_slice(trailer);

    let mut hasher = Hasher::new(algo);
    hasher.update(&out);
    let checksum = hasher.finalize();
    out.extend_from_slice(&checksum);
    out
}
