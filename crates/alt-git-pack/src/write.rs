//! Pack/idx writing (version 2), plain entries only: a pack without
//! deltas is a fully valid git pack, and L1 fidelity needs identity, not
//! layout — delta'ing the export is a volume optimization that belongs to
//! a later batch.
//!
//! The pack streams through a hashing writer (the trailer is the running
//! digest) with a per-entry crc32 for the idx; both files are written to
//! temp names and renamed to `pack-<trailer-hex>.{pack,idx}` at finish.

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::PackError;

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
    crc: Option<flate2::Crc>,
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
        })
    }

    /// Writes one plain entry (`kind` + zlib payload). The caller vouches
    /// that `oid` is the object's id; export computes it structurally.
    pub fn add(&mut self, oid: ObjectId, kind: ObjectKind, data: &[u8]) -> Result<(), PackError> {
        let offset = self.out.written;
        self.out.crc = Some(flate2::Crc::new());

        // entry header: 3-bit type + size varint (low 4 bits first)
        let type_id: u8 = match kind {
            ObjectKind::Commit => 1,
            ObjectKind::Tree => 2,
            ObjectKind::Blob => 3,
            ObjectKind::Tag => 4,
        };
        let mut header = Vec::with_capacity(10);
        let mut size = data.len() as u64;
        let mut byte = (type_id << 4) | (size & 0xf) as u8;
        size >>= 4;
        while size > 0 {
            header.push(byte | 0x80);
            byte = (size & 0x7f) as u8;
            size >>= 7;
        }
        header.push(byte);
        self.out.write_all(&header)?;

        let mut encoder = ZlibEncoder::new(&mut self.out, Compression::default());
        encoder.write_all(data)?;
        encoder.finish()?;

        let crc = self.out.crc.take().expect("entry crc active").sum();
        self.entries.push((oid, offset, crc));
        Ok(())
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
