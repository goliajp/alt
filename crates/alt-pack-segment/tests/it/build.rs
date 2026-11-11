//! Segments written object by object, and objects read back as streams.
use super::*;
use alt_pack_segment::{Builder, Error, Kind, build_pack};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};

/// a new file a builder can write and read back
fn file(path: &Path) -> fs::File {
    fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap()
}

fn id(kind: Kind, data: &[u8]) -> [u8; 20] {
    let mut hasher = alt_hash::Sha1::new();
    hasher.update(format!("{} {}\0", kind.name(), data.len()).as_bytes());
    hasher.update(data);
    hasher.finalize()
}

fn hex(oid: &[u8; 20]) -> String {
    alt_pack_segment::hex(oid)
}

/// deterministic bytes that do not compress to nothing
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|at| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            if at % 3 == 0 { b'a' } else { state as u8 }
        })
        .collect()
}

fn tree(entries: &[(&str, &str, [u8; 20])]) -> Vec<u8> {
    let mut data = Vec::new();
    for (mode, name, oid) in entries {
        data.extend_from_slice(format!("{mode} {name}\0").as_bytes());
        data.extend_from_slice(oid);
    }
    data
}

fn commit(tree: &[u8; 20], parents: &[[u8; 20]], message: &str) -> Vec<u8> {
    let parents: String = parents
        .iter()
        .map(|parent| format!("parent {}\n", hex(parent)))
        .collect();
    format!(
        "tree {}\n{parents}author A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\n{message}\n",
        hex(tree)
    )
    .into_bytes()
}

/// `commits` commits in a line, each with a tree of `files` new files of `bytes` bytes,
/// and a tag on the last; objects come referrer first, so every reference points forward
fn history(commits: usize, files: usize, bytes: usize) -> Vec<(Kind, Vec<u8>)> {
    let mut objects = Vec::new();
    let mut parent: Option<[u8; 20]> = None;
    for at in 0..commits {
        let blobs: Vec<Vec<u8>> = (0..files)
            .map(|file| noise((at * files + file) as u64 + 1, bytes + file))
            .collect();
        let names: Vec<String> = (0..files).map(|file| format!("file-{file:04}")).collect();
        let entries: Vec<(&str, &str, [u8; 20])> = names
            .iter()
            .zip(&blobs)
            .map(|(name, blob)| ("100644", name.as_str(), id(Kind::Blob, blob)))
            .collect();
        let inner = tree(&entries);
        let outer = tree(&[("40000", "dir", id(Kind::Tree, &inner))]);
        let this = commit(
            &id(Kind::Tree, &outer),
            parent.as_slice(),
            &format!("commit {at}"),
        );
        let oid = id(Kind::Commit, &this);
        // children first in the stream, parents after: generations cannot be taken in order
        objects.insert(0, (Kind::Commit, this));
        objects.push((Kind::Tree, outer));
        objects.push((Kind::Tree, inner));
        objects.extend(blobs.into_iter().map(|blob| (Kind::Blob, blob)));
        parent = Some(oid);
    }
    let tag = format!(
        "object {}\ntype commit\ntag v1\ntagger A <a@example.test> 1 +0000\n\nv1\n",
        hex(&parent.unwrap())
    );
    objects.insert(0, (Kind::Tag, tag.into_bytes()));
    objects
}

fn built(
    dir: &Path,
    name: &str,
    objects: &[(Kind, Vec<u8>)],
    existing: &SegmentSet,
) -> Result<(Arc<Segment>, alt_pack_segment::Built<fs::File>), Error> {
    let path = dir.join(name);
    let mut builder = Builder::new(file(&path), existing, objects.len() as u64)?;
    for (kind, data) in objects {
        builder.add(*kind, data)?;
    }
    let built = builder.finish()?;
    Ok((Arc::new(Segment::open(&path).unwrap()), built))
}

fn stream(set: &SegmentSet, oid: &[u8; 20]) -> (Kind, Vec<u8>) {
    let (kind, size, mut stream) = set.open(oid).unwrap().unwrap();
    let mut data = Vec::new();
    stream.read_to_end(&mut data).unwrap();
    assert_eq!(size, data.len() as u64);
    (kind, data)
}

#[test]
fn a_built_segment_is_the_segment_of_the_same_objects_as_a_pack() {
    let work = alt_os::temp::tempdir().unwrap();
    // 24 commits of 64 files of 32 KiB each
    let objects = history(24, 64, 32 << 10);
    let total: usize = objects.iter().map(|(_, data)| data.len()).sum();
    assert!(total > 48 << 20, "{total}");
    let empty = SegmentSet::default();
    let (segment, built) = built(work.path(), "built.seg", &objects, &empty).unwrap();
    assert_eq!(built.objects, objects.len() as u64);
    assert_eq!(built.flags, 0);
    assert_eq!(
        built.bytes,
        fs::metadata(work.path().join("built.seg")).unwrap().len()
    );

    let pack = build_pack(objects.iter().map(|(kind, data)| (*kind, data.as_slice()))).unwrap();
    let packed = super::segment(work.path(), "packed.seg", &pack, &empty);
    assert_eq!(
        fs::read(work.path().join("built.seg")).unwrap(),
        fs::read(work.path().join("packed.seg")).unwrap()
    );
    let (one, other) = (
        SegmentSet::new(vec![segment]),
        SegmentSet::new(vec![packed]),
    );
    assert_eq!(one.objects(), other.objects());
    let mut newest = [0u8; 20];
    for (kind, data) in &objects {
        let oid = id(*kind, data);
        for set in [&one, &other] {
            let record = set.record(&oid).unwrap().unwrap();
            assert_eq!(
                (record.kind, record.size, record.blake3),
                (
                    *kind,
                    data.len() as u64,
                    *alt_hash::blake3::hash(data).as_bytes()
                )
            );
            assert_eq!(&set.read(&oid).unwrap().1, data);
        }
        assert_eq!(stream(&one, &oid), (*kind, data.clone()));
        if *kind == Kind::Commit && newest == [0; 20] {
            newest = oid;
        }
    }
    assert_eq!(one.commit(&newest).unwrap().generation, 24);
    assert_eq!(other.commit(&newest).unwrap().generation, 24);
    let digest = *alt_hash::blake3::hash(&objects[5].1).as_bytes();
    let (oid, kind, size, mut by_digest) = one.open_blake3(&digest).unwrap().unwrap();
    let mut data = Vec::new();
    by_digest.read_to_end(&mut data).unwrap();
    assert_eq!(
        (oid, kind, size, &data),
        (
            id(objects[5].0, &objects[5].1),
            objects[5].0,
            data.len() as u64,
            &objects[5].1
        )
    );
    assert!(one.open_blake3(&[0x5a; 32]).unwrap().is_none());
    assert!(one.open(&[0x5a; 20]).unwrap().is_none());
}

#[test]
fn a_build_settles_references_against_itself_and_the_segments_present() {
    let work = alt_os::temp::tempdir().unwrap();
    let empty = SegmentSet::default();
    let blob = b"content".to_vec();
    let root = tree(&[("100644", "file", id(Kind::Blob, &blob))]);
    let first = commit(&id(Kind::Tree, &root), &[], "first");
    let base = vec![
        (Kind::Commit, first.clone()),
        (Kind::Tree, root.clone()),
        (Kind::Blob, blob.clone()),
    ];
    let (segment, _) = built(work.path(), "base.seg", &base, &empty).unwrap();
    let present = SegmentSet::new(vec![segment]);

    // a commit on a tree and a parent held by the earlier segment
    let second = commit(
        &id(Kind::Tree, &root),
        &[id(Kind::Commit, &first)],
        "second",
    );
    let (segment, result) = built(
        work.path(),
        "next.seg",
        &[(Kind::Commit, second.clone())],
        &present,
    )
    .unwrap();
    assert_eq!((result.objects, result.flags), (1, 0));
    let both = SegmentSet::new(vec![present.segments()[0].clone(), segment]);
    assert_eq!(
        both.commit(&id(Kind::Commit, &second)).unwrap().generation,
        2
    );

    // without that segment the same commit refers to nothing
    assert!(matches!(
        built(
            work.path(),
            "dangling.seg",
            &[(Kind::Commit, second)],
            &empty
        ),
        Err(Error::Dangling(..))
    ));
    // a tree entry that names a commit's id as a file
    let mistyped = tree(&[("100644", "file", id(Kind::Commit, &first))]);
    assert!(matches!(
        built(
            work.path(),
            "mistyped.seg",
            &[(Kind::Tree, mistyped.clone())],
            &present
        ),
        Err(Error::Dangling(..))
    ));
    // the same, with the object arriving after its referrer
    assert!(matches!(
        built(
            work.path(),
            "late.seg",
            &[(Kind::Tree, mistyped), (Kind::Commit, first.clone())],
            &empty
        ),
        Err(Error::Dangling(..))
    ));

    // objects an earlier segment holds, and one handed over twice
    let (_, result) = built(
        work.path(),
        "again.seg",
        &[(Kind::Blob, blob.clone()), (Kind::Blob, blob.clone())],
        &present,
    )
    .unwrap();
    assert_eq!(result.objects, 1);
    assert_eq!(
        result.flags,
        alt_pack_segment::FLAG_DUPLICATES | alt_pack_segment::FLAG_OVERLAP
    );

    // the declared count is exact
    let path = work.path().join("count.seg");
    let mut builder = Builder::new(file(&path), &empty, 1).unwrap();
    builder.add(Kind::Blob, &blob).unwrap();
    assert!(matches!(
        builder.add(Kind::Blob, b"more"),
        Err(Error::Format("more objects than declared"))
    ));
    let builder = Builder::new(file(&path), &empty, 2).unwrap();
    assert!(matches!(
        builder.finish(),
        Err(Error::Format("fewer objects than declared"))
    ));
}

#[test]
fn objects_stored_as_deltas_stream_like_whole_ones() {
    let work = alt_os::temp::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    repository(&source, 20);
    git(&source, &["repack", "-adq", "--depth=20", "--window=20"]);
    let pack = git_in(
        &source,
        &[
            "pack-objects",
            "--all",
            "--stdout",
            "-q",
            "--delta-base-offset",
        ],
        b"",
    );
    let set = SegmentSet::new(vec![segment(
        work.path(),
        "one.seg",
        &pack,
        &SegmentSet::default(),
    )]);
    let ids = objects(&source, &["--all"]);
    let mut deltas = 0;
    for hex in &ids {
        let oid = oid(hex);
        let record = set.record(&oid).unwrap().unwrap();
        if record.depth > 0 {
            deltas += 1;
        }
        let whole = set.read(&oid).unwrap();
        assert_eq!(stream(&set, &oid), whole, "{hex}");
        // a stream outlives the set it was opened from
        let (_, _, mut kept) = set.clone().open(&oid).unwrap().unwrap();
        let mut data = Vec::new();
        kept.read_to_end(&mut data).unwrap();
        assert_eq!(data, whole.1);
    }
    assert!(deltas > 0);
}

fn zlib(data: &[u8]) -> Vec<u8> {
    alt_deflate::compress(data, 1, alt_deflate::Format::Zlib)
}

/// the compressors below go wrong for objects that begin like this
const MARK: &[u8] = b"marked";

fn marked(data: &[u8]) -> bool {
    data.starts_with(MARK)
}

/// a stream that ends before its checksum
fn cut(data: &[u8]) -> Vec<u8> {
    let mut stream = zlib(data);
    if marked(data) {
        stream.truncate(stream.len() - 4);
    }
    stream
}

/// a stream damaged in the middle
fn flipped(data: &[u8]) -> Vec<u8> {
    let mut stream = zlib(data);
    if marked(data) {
        let middle = stream.len() / 2;
        stream[middle] ^= 0x10;
    }
    stream
}

/// a sound stream of other content of the same length
fn other(data: &[u8]) -> Vec<u8> {
    if !marked(data) {
        return zlib(data);
    }
    let mut changed = data.to_vec();
    *changed.last_mut().unwrap() ^= 1;
    zlib(&changed)
}

/// a sound stream of content one byte shorter
fn shorter(data: &[u8]) -> Vec<u8> {
    zlib(&data[..data.len() - usize::from(marked(data))])
}

/// a sound stream with a byte after its end
fn padded(data: &[u8]) -> Vec<u8> {
    let mut stream = zlib(data);
    if marked(data) {
        stream.push(0);
    }
    stream
}

#[test]
fn a_build_fails_when_a_written_object_does_not_read_back() {
    let work = alt_os::temp::tempdir().unwrap();
    let empty = SegmentSet::default();
    let path = work.path().join("built.seg");
    // one object of a few bytes and one that is read back in several chunks
    for size in [16, 300 << 10] {
        let mut bad = MARK.to_vec();
        bad.extend(noise(7, size));
        let objects = [
            (Kind::Blob, noise(1, 4096)),
            (Kind::Blob, bad),
            (Kind::Blob, noise(2, 100 << 10)),
        ];
        let compressors: [(&str, alt_pack_segment::Compress); 5] = [
            ("cut", cut),
            ("flipped", flipped),
            ("other", other),
            ("shorter", shorter),
            ("padded", padded),
        ];
        for (name, compress) in compressors {
            let mut builder = Builder::compressing(file(&path), &empty, 3, compress).unwrap();
            for (kind, data) in &objects {
                builder.add(*kind, data).unwrap();
            }
            assert!(
                matches!(
                    builder.finish(),
                    Err(Error::Corrupt(alt_pack_segment::UNREADABLE))
                ),
                "{name} {size}"
            );
        }
        // the same objects through a sound compressor
        let mut builder = Builder::compressing(file(&path), &empty, 3, zlib).unwrap();
        for (kind, data) in &objects {
            builder.add(*kind, data).unwrap();
        }
        assert_eq!(builder.finish().unwrap().objects, 3);
    }
}

/// storage that keeps one byte other than the one written to it
struct Faulty {
    inner: Cursor<Vec<u8>>,
    at: u64,
}

impl Write for Faulty {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let position = self.inner.position();
        let mut bytes = bytes.to_vec();
        if (position..position + bytes.len() as u64).contains(&self.at) {
            bytes[(self.at - position) as usize] ^= 0x40;
        }
        self.inner.write(&bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for Faulty {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(output)
    }
}

impl Seek for Faulty {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

#[test]
fn a_build_fails_when_the_bytes_stored_are_not_the_bytes_written() {
    let empty = SegmentSet::default();
    let objects = [
        (Kind::Blob, noise(1, 4096)),
        (Kind::Blob, noise(2, 100 << 10)),
        (Kind::Blob, noise(3, 4096)),
    ];
    // the segment follows whatever the output already holds
    let start = 7;
    let build = |at: u64| {
        let mut inner = Cursor::new(vec![0xee; start]);
        inner.seek(SeekFrom::End(0)).unwrap();
        let mut builder = Builder::new(Faulty { inner, at }, &empty, 3).unwrap();
        for (kind, data) in &objects {
            builder.add(*kind, data).unwrap();
        }
        builder.finish()
    };
    let built = build(u64::MAX).unwrap();
    let whole = built.out.inner.into_inner();
    assert_eq!(whole.len() as u64, start as u64 + built.bytes);
    let path = alt_os::temp::tempdir().unwrap();
    fs::write(path.path().join("built.seg"), &whole[start..]).unwrap();
    let set = SegmentSet::new(vec![Arc::new(
        Segment::open(&path.path().join("built.seg")).unwrap(),
    )]);
    // a byte of each entry in turn: its header, then somewhere in its stream
    for (kind, data) in &objects {
        let oid = id(*kind, data);
        let record = set.record(&oid).unwrap().unwrap();
        for within in [0, u64::from(record.len) / 2, u64::from(record.len) - 1] {
            assert!(
                matches!(
                    build(start as u64 + record.offset + within),
                    Err(Error::Corrupt(alt_pack_segment::UNREADABLE))
                ),
                "{within}"
            );
        }
    }
}
