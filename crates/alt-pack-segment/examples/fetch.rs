//! Times the two halves of a fetch that is not the whole repository: choosing the
//! objects, and writing them as a pack.
//!
//! `fetch build <pack> <segment>` verifies a pack and writes it as a segment file.
//! `fetch run <segment> [thin] [select] <want>... [^<have>...]` selects and writes, and
//! prints the time of each half, the objects chosen and the bytes written; with
//! `select` nothing is written.

use alt_pack_segment::{Limits, Oid, Segment, SegmentSet, ingest};
use std::{
    io::Write,
    sync::Arc,
    time::{Duration, Instant},
};

fn oid(hex: &str) -> Oid {
    let mut out = [0; 20];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
    }
    out
}

struct Counted(u64);

impl Write for Counted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len() as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn ms(time: Duration) -> f64 {
    time.as_secs_f64() * 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => {
            let pack = std::fs::read(&args[1]).unwrap();
            let limits = Limits {
                delta_depth: 4096,
                ..Limits::of(1 << 28, 1 << 32, 1 << 40)
            };
            let ingested = ingest(&pack, &SegmentSet::default(), &limits).unwrap();
            let mut file = std::fs::File::create(&args[2]).unwrap();
            file.write_all(&pack).unwrap();
            ingested.write_index(&mut file).unwrap();
            println!("objects {}", ingested.objects());
        }
        Some("run") => {
            let set = SegmentSet::new(vec![Arc::new(Segment::open(args[1].as_ref()).unwrap())]);
            let thin = args.iter().any(|arg| arg == "thin");
            let only_select = args.iter().any(|arg| arg == "select");
            let ids = || args[2..].iter().filter(|arg| arg.len() >= 40);
            let wants: Vec<Oid> = ids()
                .filter(|id| !id.starts_with('^'))
                .map(|id| oid(id))
                .collect();
            let haves: Vec<Oid> = ids()
                .filter_map(|id| id.strip_prefix('^'))
                .map(oid)
                .collect();
            let start = Instant::now();
            let selection = set.select(&wants, &haves).unwrap();
            let selected = start.elapsed();
            let mut out = Counted(0);
            let start = Instant::now();
            if !only_select {
                set.write_selected(&selection, thin, &mut out).unwrap();
            }
            println!(
                "objects {} of {} select_ms {:.0} write_ms {:.0} bytes {}",
                selection.len(),
                set.objects(),
                ms(selected),
                ms(start.elapsed()),
                out.0
            );
        }
        _ => panic!("fetch build <pack> <segment> | fetch run <segment> [thin] [select] <want>..."),
    }
}
