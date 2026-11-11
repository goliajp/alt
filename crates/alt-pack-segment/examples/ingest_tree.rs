//! Times verification of a generated pack shaped like a wide working tree with
//! history: ten thousand files in a hundred directories, then 4200 commits that each
//! rewrite ten files. A little over a hundred thousand objects, and every commit
//! brings eleven trees of a hundred entries whose references have to be looked up.
//!
//! `ingest_tree whole [rounds]` verifies everything as one pack into an empty set.
//! `ingest_tree split [rounds]` verifies the second half of the history against a
//! segment that holds the first half, so most references resolve in that segment.
//! `ingest_tree build [rounds]` writes the same objects one by one into a segment
//! file, the way content that did not arrive as a pack is stored.

use alt_pack_segment::{Builder, Kind, Limits, Oid, Segment, SegmentSet, ingest};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

const COMMITS: usize = 4201;

fn object_id(kind: Kind, data: &[u8]) -> Oid {
    let mut hasher = alt_hash::Sha1::new();
    hasher.update(format!("{} {}\0", kind.name(), data.len()).as_bytes());
    hasher.update(data);
    hasher.finalize()
}

fn hex(oid: &Oid) -> String {
    oid.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 512 bytes that differ for every seed and do not compress
fn blob(seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut out = Vec::with_capacity(512);
    while out.len() < 512 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out
}

fn tree(entries: &BTreeMap<String, (bool, Oid)>) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, (directory, oid)) in entries {
        out.extend_from_slice(if *directory { b"40000 " } else { b"100644 " });
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(oid);
    }
    out
}

/// the objects each commit adds, the commit itself last
fn history() -> Vec<Vec<(Kind, Vec<u8>)>> {
    let mut directories: Vec<BTreeMap<String, (bool, Oid)>> = vec![BTreeMap::new(); 100];
    let mut root: BTreeMap<String, (bool, Oid)> = BTreeMap::new();
    let mut parent: Option<Oid> = None;
    let mut all = Vec::with_capacity(COMMITS);
    for commit in 0..COMMITS {
        let mut added = Vec::new();
        let changes: Vec<(usize, usize, u64)> = if commit == 0 {
            (0..10_000)
                .map(|file| (file / 100, file % 100, file as u64))
                .collect()
        } else {
            (0..10)
                .map(|k| {
                    (
                        (commit * 7 + k * 13) % 100,
                        (commit + k * 31) % 100,
                        (commit * 16 + k) as u64 + 1_000_000,
                    )
                })
                .collect()
        };
        let mut touched = Vec::new();
        for (directory, file, seed) in changes {
            let data = blob(seed);
            let oid = object_id(Kind::Blob, &data);
            added.push((Kind::Blob, data));
            directories[directory].insert(format!("f{file:03}.txt"), (false, oid));
            touched.push(directory);
        }
        touched.sort_unstable();
        touched.dedup();
        for directory in touched {
            let data = tree(&directories[directory]);
            let oid = object_id(Kind::Tree, &data);
            added.push((Kind::Tree, data));
            root.insert(format!("d{directory:03}"), (true, oid));
        }
        if commit == 0 {
            let data = b"the file the later commits leave alone\n".to_vec();
            root.insert(
                "source.txt".to_owned(),
                (false, object_id(Kind::Blob, &data)),
            );
            added.push((Kind::Blob, data));
        }
        let data = tree(&root);
        let root_oid = object_id(Kind::Tree, &data);
        added.push((Kind::Tree, data));
        let mut text = format!("tree {}\n", hex(&root_oid));
        if let Some(parent) = &parent {
            text.push_str(&format!("parent {}\n", hex(parent)));
        }
        let stamp = 1_700_000_000 + commit;
        text.push_str(&format!(
            "author t <t@example.test> {stamp} +0000\ncommitter t <t@example.test> {stamp} +0000\n\ncommit {commit}\n"
        ));
        let data = text.into_bytes();
        parent = Some(object_id(Kind::Commit, &data));
        added.push((Kind::Commit, data));
        all.push(added);
    }
    all
}

/// A pack of these commits' objects, each stored without compression: the zlib
/// streams are made of stored blocks, so the time measured is that of verification.
fn pack(commits: &[Vec<(Kind, Vec<u8>)>]) -> Vec<u8> {
    let count: usize = commits.iter().map(Vec::len).sum();
    let mut out = b"PACK\0\0\0\x02".to_vec();
    out.extend_from_slice(&(count as u32).to_be_bytes());
    for (kind, data) in commits.iter().flatten() {
        let mut size = data.len() >> 4;
        let mut byte = (*kind as u8) << 4 | (data.len() & 15) as u8;
        while size != 0 {
            out.push(byte | 0x80);
            byte = (size & 0x7f) as u8;
            size >>= 7;
        }
        out.push(byte);
        out.extend_from_slice(&[0x78, 0x01]);
        let mut blocks = data.chunks(0xffff).peekable();
        while let Some(block) = blocks.next() {
            out.push(u8::from(blocks.peek().is_none()));
            out.extend_from_slice(&(block.len() as u16).to_le_bytes());
            out.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            out.extend_from_slice(block);
        }
        out.extend_from_slice(&alt_hash::adler32(1, data).to_be_bytes());
    }
    let trailer = alt_hash::sha1(&out);
    out.extend_from_slice(&trailer);
    out
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "whole".to_owned());
    let rounds: usize = args.next().map_or(7, |value| value.parse().unwrap());
    let limits = Limits {
        objects: 1 << 24,
        object_bytes: 1 << 32,
        inflated_bytes: 1 << 40,
        delta_depth: 4096,
    };
    let commits = history();
    let work = alt_os::temp::tempdir().unwrap();
    if mode == "build" {
        return build(&commits, work.path(), rounds);
    }
    let (existing, measured) = match mode.as_str() {
        "whole" => (SegmentSet::default(), pack(&commits)),
        "split" => {
            let (first, second) = commits.split_at(COMMITS / 2);
            let base = pack(first);
            let path = work.path().join("base.seg");
            let mut file = std::fs::File::create(&path).unwrap();
            std::io::Write::write_all(&mut file, &base).unwrap();
            ingest(&base, &SegmentSet::default(), &limits)
                .unwrap()
                .write_index(&mut file)
                .unwrap();
            drop(file);
            let existing = SegmentSet::new(vec![Arc::new(Segment::open(&path).unwrap())]);
            (existing, pack(second))
        }
        other => panic!("unknown mode {other}"),
    };
    let mut times = Vec::new();
    let mut objects = 0;
    let mut index = Vec::new();
    for _ in 0..rounds {
        let start = Instant::now();
        let ingested = ingest(&measured, &existing, &limits).unwrap();
        index.clear();
        ingested.write_index(&mut index).unwrap();
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        objects = ingested.objects();
    }
    times.sort_by(f64::total_cmp);
    println!(
        "{mode}: objects {objects} existing {} pack_bytes {} index_blake3 {} median_ms {:.1} min_ms {:.1}",
        existing.objects(),
        measured.len(),
        alt_hash::blake3::hash(&index),
        times[times.len() / 2],
        times[0]
    );
}

fn build(commits: &[Vec<(Kind, Vec<u8>)>], work: &std::path::Path, rounds: usize) {
    // a commit comes after everything it refers to; the builder takes any order
    let objects: Vec<&(Kind, Vec<u8>)> = commits.iter().flatten().collect();
    let existing = SegmentSet::default();
    let path = work.join("built.seg");
    let mut times = Vec::new();
    let (mut count, mut bytes) = (0, 0);
    for _ in 0..rounds {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let start = Instant::now();
        let mut builder = Builder::new(file, &existing, objects.len() as u64).unwrap();
        for (kind, data) in &objects {
            builder.add(*kind, data).unwrap();
        }
        let built = builder.finish().unwrap();
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        (count, bytes) = (built.objects, built.bytes);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "build: objects {count} segment_bytes {bytes} median_ms {:.1} min_ms {:.1}",
        times[times.len() / 2],
        times[0]
    );
}
