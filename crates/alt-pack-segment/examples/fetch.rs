//! Times the two halves of a fetch that is not the whole repository: choosing the
//! objects, and writing them as a pack.
//!
//! `fetch build <pack> <segment>` verifies a pack and writes it as a segment file,
//! and tells the most memory the verifying was counted for beside the most the
//! process had resident beyond the pack itself.
//! `fetch run <segment> [thin] [select] <want>... [^<have>...]` selects and writes, and
//! prints the time of each half, the objects chosen and the bytes written; with
//! `select` nothing is written.
//! `fetch reach <segment> <tip> <said>...` tells which of the commits said the tip
//! leads to, and the time that took.
//! `fetch held <segment> <bytes> <want>...` selects and writes as `run` does, and
//! tells what the process holds in memory, beyond what it held before the
//! selection, once `bytes` of the pack are written: what a fetch whose response
//! stops there to wait holds while it waits.

use alt_pack_segment::{Limits, Memory, Oid, Segment, SegmentSet, ingest};
use std::{
    io::Write,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// the bytes of this process that are in memory now, as Linux tells them
fn resident() -> u64 {
    let told = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages = told.split(' ').nth(1).and_then(|pages| pages.parse().ok());
    pages.unwrap_or(0u64) * 4096
}

/// the bytes of this process that are in memory and are no file's, as Linux tells them
fn anonymous() -> u64 {
    let told = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: Vec<u64> = told
        .split(' ')
        .filter_map(|pages| pages.parse().ok())
        .collect();
    pages
        .get(1)
        .zip(pages.get(2))
        .map_or(0, |(all, shared)| (all - shared) * 4096)
}

fn oid(hex: &str) -> Oid {
    let mut out = [0; 20];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
    }
    out
}

struct Counted(u64);

/// counts what is written, and notes what the process holds once `at` bytes are
struct Held {
    written: u64,
    at: u64,
    noted: Option<(u64, u64)>,
}

impl Write for Held {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.written += bytes.len() as u64;
        if self.noted.is_none() && self.written >= self.at {
            self.noted = Some((resident(), anonymous()));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

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
            let memory: &'static Memory = Box::leak(Box::new(Memory::new(1 << 40)));
            let limits = Limits {
                delta_depth: 4096,
                memory: Some(memory),
                ..Limits::of(1 << 28, 1 << 32, 1 << 40)
            };
            let (before, done) = (resident(), AtomicBool::new(false));
            let (ingested, most) = std::thread::scope(|scope| {
                let watching = scope.spawn(|| {
                    let (mut counted, mut held) = (0, 0);
                    while !done.load(Ordering::Relaxed) {
                        let taken = memory.taken();
                        counted = counted.max(taken.0 + taken.1);
                        held = held.max(resident());
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    (counted, held)
                });
                let ingested = ingest(&pack, &SegmentSet::default(), &limits).unwrap();
                done.store(true, Ordering::Relaxed);
                (ingested, watching.join().unwrap())
            });
            println!(
                "counted {} resident_beyond_the_pack {}",
                most.0,
                most.1.saturating_sub(before)
            );
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
        Some("held") => {
            let set = SegmentSet::new(vec![Arc::new(Segment::open(args[1].as_ref()).unwrap())]);
            let at: u64 = args[2].parse().unwrap();
            let wants: Vec<Oid> = args[3..].iter().map(|id| oid(id)).collect();
            let before = (resident(), anonymous());
            let start = Instant::now();
            let selection = set.select(&wants, &[]).unwrap();
            let selected = start.elapsed();
            let mut out = Held {
                written: 0,
                at,
                noted: None,
            };
            set.write_selected(&selection, false, &mut out).unwrap();
            let noted = out.noted.unwrap_or_default();
            println!(
                "objects {} select_ms {:.0} bytes {} at {} resident_beyond {} anonymous_beyond {}",
                selection.len(),
                ms(selected),
                out.written,
                at,
                noted.0.saturating_sub(before.0),
                noted.1.saturating_sub(before.1)
            );
        }
        Some("reach") => {
            let set = SegmentSet::new(vec![Arc::new(Segment::open(args[1].as_ref()).unwrap())]);
            let said: Vec<Oid> = args[3..].iter().map(|id| oid(id)).collect();
            let start = Instant::now();
            let found = set.reached(&[oid(&args[2])], &said, usize::MAX).unwrap();
            println!(
                "reached {} of {} reach_ms {:.1}",
                found.len(),
                said.len(),
                ms(start.elapsed())
            );
        }
        _ => panic!("fetch build <pack> <segment> | fetch run <segment> [thin] [select] <want>..."),
    }
}
