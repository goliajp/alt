//! times verification of a pack file: `ingest <pack> [rounds] [bytes of bases held in memory]`

use alt_pack_segment::{Limits, SegmentSet, ingest};
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let pack = std::fs::read(args.next().expect("pack path")).unwrap();
    let rounds: usize = args.next().map_or(15, |value| value.parse().unwrap());
    let mut limits = Limits {
        delta_depth: 4096,
        ..Limits::of(1 << 24, 1 << 32, 1 << 40)
    };
    if let Some(held) = args.next() {
        limits.base_bytes = held.parse().unwrap();
    }
    let mut work = Default::default();
    let mut times = Vec::new();
    let mut objects = 0;
    for _ in 0..rounds {
        let start = Instant::now();
        let ingested = ingest(&pack, &SegmentSet::default(), &limits).unwrap();
        let mut index = Vec::new();
        ingested.write_index(&mut index).unwrap();
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        objects = ingested.objects();
        work = ingested.work();
    }
    times.sort_by(f64::total_cmp);
    println!(
        "objects {objects} pack_bytes {} median_ms {:.2} min_ms {:.2}",
        pack.len(),
        times[times.len() / 2],
        times[0]
    );
    println!("bases_held {} bases_in_files {}", work.held, work.spilled);
}
