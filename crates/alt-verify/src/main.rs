//! Re-hashes every object — loose and packed — of the given repositories,
//! pairs every read with an export (parse → serialize must reproduce the
//! bytes), and reports throughput. The M1 verification harness and bench
//! baseline.
//!
//! ```sh
//! cargo run --release -p alt-verify -- <repo>…
//! ```

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use alt_git_codec::{Commit, HashAlgo, LooseStore, ObjectId, ObjectKind, RawObject, Tag, Tree};
use alt_git_pack::IndexedPack;
use rayon::prelude::*;

/// Read ↔ export pairing: every parsed object must serialize back to the
/// exact bytes it was read from.
fn verify_export(algo: HashAlgo, oid: &ObjectId, raw: &RawObject) {
    let reserialized = match raw.kind {
        ObjectKind::Blob => return,
        ObjectKind::Commit => Commit::parse(&raw.data).unwrap().serialize(),
        ObjectKind::Tree => Tree::parse(&raw.data, algo).unwrap().serialize(),
        ObjectKind::Tag => Tag::parse(&raw.data).unwrap().serialize(),
    };
    assert_eq!(reserialized, raw.data, "export mismatch for {oid}");
}

fn main() {
    let repos: Vec<String> = std::env::args().skip(1).collect();
    if repos.is_empty() {
        eprintln!("usage: alt-verify <repo>…");
        exit(2);
    }
    for repo in &repos {
        verify_repo(Path::new(repo));
    }
}

fn verify_repo(repo: &Path) {
    // corpus repos are SHA-1 until S9 wires config detection
    let algo = HashAlgo::Sha1;
    let objects = repo.join(".git/objects");
    let bytes = AtomicU64::new(0);
    let started = Instant::now();

    // loose objects, in parallel
    let store = LooseStore::new(&objects);
    let loose = list_loose(&objects);
    loose.par_iter().for_each(|oid| {
        let raw = store.read(oid).unwrap();
        assert_eq!(
            ObjectId::hash_object(algo, raw.kind, &raw.data),
            *oid,
            "loose re-hash mismatch in {repo:?}"
        );
        verify_export(algo, oid, &raw);
        bytes.fetch_add(raw.data.len() as u64, Ordering::Relaxed);
    });

    // every entry of every pack, in parallel
    let mut packed = 0u64;
    for pack_path in list_packs(&objects.join("pack")) {
        let indexed = IndexedPack::open(&pack_path, algo).unwrap();
        let idx = indexed.idx();
        packed += u64::from(idx.len());
        // ascending pack offset: bases precede their deltas, so the chain
        // cache works with the pack's own layout instead of against it
        let mut order: Vec<(u64, u32)> = (0..idx.len())
            .map(|i| (idx.offset_at(i).unwrap(), i))
            .collect();
        order.sort_unstable();
        order.par_iter().for_each(|&(offset, i)| {
            let oid = idx.oid_at(i);
            let obj = indexed.read_at(offset).unwrap();
            assert_eq!(
                ObjectId::hash_object(algo, obj.kind, &obj.data),
                oid,
                "packed re-hash mismatch in {pack_path:?}"
            );
            verify_export(
                algo,
                &oid,
                &RawObject {
                    kind: obj.kind,
                    data: (*obj.data).clone(),
                },
            );
            bytes.fetch_add(obj.data.len() as u64, Ordering::Relaxed);
        });
    }

    let secs = started.elapsed().as_secs_f64();
    let total = loose.len() as u64 + packed;
    let mb = bytes.load(Ordering::Relaxed) as f64 / 1e6;
    println!(
        "{}: {total} objects ({} loose + {packed} packed), {mb:.1} MB inflated, \
         {secs:.2}s, {:.0} obj/s",
        repo.display(),
        loose.len(),
        total as f64 / secs,
    );
}

fn list_loose(objects: &Path) -> Vec<ObjectId> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(objects) else {
        return out;
    };
    for entry in entries {
        let entry = entry.unwrap();
        let fanout = entry.file_name().into_string().unwrap();
        if fanout.len() != 2 {
            continue; // info/, pack/
        }
        for obj in std::fs::read_dir(entry.path()).unwrap() {
            let rest = obj.unwrap().file_name().into_string().unwrap();
            out.push(ObjectId::from_hex(format!("{fanout}{rest}").as_bytes()).unwrap());
        }
    }
    out
}

fn list_packs(pack_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(pack_dir) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pack"))
        .collect()
}
