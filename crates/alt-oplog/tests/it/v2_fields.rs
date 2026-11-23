//! Version 2 logs in which one number says something else than a writer
//! laid, in a frame or a header whose checksum holds: reading them ends
//! in what the log holds or in an error.

use std::fs;
use std::path::Path;

use alt_oplog::v2::block::{FileHeader, Layout};
use alt_oplog::v2::checkpoint::Checkpoint;
use alt_oplog::v2::frame::{Ext, Head, KIND_CHECKPOINT, Seed, encode};
use alt_oplog::v2::scan::Walk;
use alt_oplog::v2::state::State;
use alt_oplog::{OpId, OpLog, OpLogError};

use super::v2_log::{boot, grow, new_log};
use super::v2_untrusted::{anchors, read_through, varied};

struct Laid {
    at: u64,
    end: u64,
    head: Head,
    ext: Ext,
    body: Vec<u8>,
    op: Option<OpId>,
    generation: u32,
}

/// The frames of the log `bytes` are, each with the generation it was
/// written under.
fn laid(bytes: &[u8]) -> (Layout, Vec<Laid>) {
    let layout = FileHeader::decode(bytes).unwrap().layout();
    let (mut generation, mut frames) = (0, Vec::new());
    let walk = Walk {
        layout,
        data: bytes,
        base: 0,
        strict: false,
    };
    let walked = walk.run(&mut State::empty(), &mut |at, end, view| {
        if view.head.kind == KIND_CHECKPOINT {
            generation = Checkpoint::decode(view.body).unwrap().generation;
        }
        frames.push(Laid {
            at,
            end,
            head: view.head,
            ext: view.ext.clone(),
            body: view.body.to_vec(),
            op: view.op,
            generation,
        });
        Ok(())
    });
    walked.unwrap();
    (layout, frames)
}

/// What a number is changed to in a file of `len` bytes.
fn values(len: u64, at: u64) -> [u64; 10] {
    let far = u64::MAX;
    [
        0,
        1,
        at,
        at + 1,
        len - 1,
        len,
        len + 1,
        1 << 32,
        far - 10,
        far,
    ]
}

impl Laid {
    fn of(other: &Laid) -> Laid {
        Laid {
            ext: other.ext.clone(),
            body: other.body.clone(),
            ..*other
        }
    }

    /// The frame with `head` and `body` in place of its own, where it
    /// lies in one piece; its checksum holds.
    fn with(&self, layout: &Layout, head: &Head, body: &[u8]) -> Option<Vec<u8>> {
        let seed = Seed::new(&layout.id, self.generation);
        let frame = encode(&seed, head, &self.ext, body, self.op.as_ref()).unwrap();
        (frame.len() as u64 == self.end - self.at).then_some(frame)
    }
}

const IN_HEAD: [fn(&mut Head, u64); 4] = [
    |head, to| head.prev = to,
    |head, to| head.prev_in_workspace = to,
    |head, to| head.synced = to,
    |head, to| head.seq = to,
];

const IN_CHECKPOINT: [fn(&mut Checkpoint, u64); 12] = [
    |said, to| said.generation = to as u32,
    |said, to| said.seq = to,
    |said, to| said.head_at = to,
    |said, to| said.head_end = to,
    |said, to| said.synced = to,
    |said, to| said.linked_at = to,
    |said, to| said.linked = to,
    |said, to| said.migrated = to,
    |said, to| said.workspaces[0].last = to,
    |said, to| said.workspaces[0].undoable = to,
    |said, to| said.workspaces[0].index_at = to,
    |said, to| said.workspaces[0].step = to,
];

/// Every log that differs from `whole` in one number of one frame.
fn each_changed_frame(whole: &[u8], mut with: impl FnMut(Vec<u8>, String)) {
    let (layout, frames) = laid(whole);
    let len = whole.len() as u64;
    let put = |at: u64, bytes: &[u8]| {
        let mut changed = whole.to_vec();
        changed[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
        changed
    };
    for (n, frame) in frames.iter().enumerate() {
        for to in values(len, frame.at) {
            for (field, set) in IN_HEAD.iter().enumerate() {
                let mut head = frame.head;
                set(&mut head, to);
                if let Some(bytes) = frame.with(&layout, &head, &frame.body) {
                    with(
                        put(frame.at, &bytes),
                        format!("frame {n} field {field} = {to}"),
                    );
                }
            }
            let said =
                Checkpoint::decode(&frame.body).filter(|_| frame.head.kind == KIND_CHECKPOINT);
            for (field, set) in IN_CHECKPOINT.iter().enumerate() {
                let Some(mut said) = said.clone().filter(|said| !said.workspaces.is_empty()) else {
                    break;
                };
                set(&mut said, to);
                if let Some(bytes) = frame.with(&layout, &frame.head, &said.encode()) {
                    with(
                        put(frame.at, &bytes),
                        format!("checkpoint {n} field {field} = {to}"),
                    );
                }
            }
            // the lengths the frame gives of itself, its checksum left as it was
            with(
                put(frame.at + 2, &(to as u16).to_le_bytes()),
                format!("frame {n} extension of {to}"),
            );
            with(
                put(frame.at + 4, &(to as u32).to_le_bytes()),
                format!("frame {n} body of {to}"),
            );
        }
    }
}

/// Every log that differs from `whole` in one number of one block header.
fn each_changed_header(whole: &[u8], mut with: impl FnMut(Vec<u8>, String)) {
    let layout = FileHeader::decode(whole).unwrap().layout();
    let len = whole.len() as u64;
    for at in (layout.block..len).step_by(layout.block as usize) {
        let header = layout.decode_header(&whole[at as usize..], at).unwrap();
        for to in values(len, at) {
            let mut changed = [header; 5];
            changed[0].index = to as u32;
            changed[1].first = to as u32;
            changed[2].checkpoint = to;
            changed[3].generation = to as u32;
            // no frame begins in the block: the checkpoint it names is gone to
            (changed[4].first, changed[4].checkpoint) = (0, to);
            for (field, header) in changed.iter().enumerate() {
                let mut bytes = whole.to_vec();
                bytes[at as usize..at as usize + 32].copy_from_slice(&layout.encode_header(header));
                with(bytes, format!("block at {at} field {field} = {to}"));
            }
        }
    }
}

/// `tail` as it says `fields` of the log in boot `in_boot`.
fn tail(in_boot: u8, generation: u32, fields: [u64; 3]) -> [u8; 64] {
    let mut bytes = [0u8; 64];
    bytes[..4].copy_from_slice(b"ALTT");
    bytes[8..24].copy_from_slice(&[in_boot; 16]);
    bytes[24..28].copy_from_slice(&generation.to_le_bytes());
    for (n, field) in fields.iter().enumerate() {
        bytes[32 + n * 8..40 + n * 8].copy_from_slice(&field.to_le_bytes());
    }
    let sum = alt_hash::blake3::hash(&bytes[..56]);
    bytes[56..].copy_from_slice(&sum.as_bytes()[..8]);
    bytes
}

fn written(dir: &Path) -> (Vec<u8>, Vec<u8>) {
    let read = |name: &str| fs::read(dir.join(name)).unwrap();
    (read("log"), read("tail"))
}

#[test]
fn a_log_with_any_number_changed_under_a_checksum_that_holds_is_read_or_refused() {
    let dir = alt_os::temp::tempdir().unwrap();
    varied(dir.path());
    let (whole, _) = written(dir.path());
    let anchors = anchors(dir.path());
    let mut cases = 0;
    let mut read = |changed: Vec<u8>, case: String| {
        fs::write(dir.path().join("log"), changed).unwrap();
        // in the boot that wrote its end, and after the machine went down
        for in_boot in [2, 3] {
            boot(in_boot);
            read_through(dir.path(), &anchors, &format!("{case} in boot {in_boot}"));
        }
        cases += 1;
    };
    each_changed_frame(&whole, &mut read);
    each_changed_header(&whole, &mut read);
    for exp in [11, 30] {
        let header = FileHeader::decode(&whole).unwrap();
        let mut changed = whole.clone();
        let other = FileHeader {
            block_exp: exp,
            ..header
        };
        changed[..64].copy_from_slice(&other.encode());
        read(changed, format!("blocks of 2^{exp}"));
    }
    assert!(cases > 2000, "{cases}");
}

#[test]
fn a_note_beside_the_log_that_says_any_number_is_read_with_or_refused() {
    let dir = alt_os::temp::tempdir().unwrap();
    varied(dir.path());
    let (whole, noted) = written(dir.path());
    let anchors = anchors(dir.path());
    let word = |at: usize| u64::from_le_bytes(noted[at..at + 8].try_into().unwrap());
    let said = [word(32), word(40), word(48)];
    let len = whole.len() as u64;
    for field in 0..4 {
        for to in values(len, said[1].min(len)) {
            let (mut generation, mut fields) = (1, said);
            match field {
                0 => generation = to as u32,
                n => fields[n - 1] = to,
            }
            fs::write(dir.path().join("tail"), tail(2, generation, fields)).unwrap();
            read_through(dir.path(), &anchors, &format!("note field {field} = {to}"));
        }
    }
}

#[test]
fn a_frame_is_looked_up_at_no_more_cost_under_a_checkpoint_of_the_last_generation_there_is() {
    let dir = alt_os::temp::tempdir().unwrap();
    boot(1);
    let mut log = new_log(dir.path());
    let anchors = grow(&mut log, "a", 5);
    log.append("a", 9, &vec![9; 20_000]).unwrap();
    log.sync().unwrap();
    drop(log);
    let (mut whole, _) = written(dir.path());
    let (layout, frames) = laid(&whole);
    // the checkpoint after the long frame, which an open begins at
    let last = frames.last().unwrap();
    let mut said = Checkpoint::decode(&last.body).unwrap();
    said.generation = u32::MAX;
    let forged = Laid {
        generation: u32::MAX,
        body: said.encode(),
        ..Laid::of(last)
    };
    let bytes = forged.with(&layout, &last.head, &forged.body).unwrap();
    whole[last.at as usize..last.end as usize].copy_from_slice(&bytes);
    // and a frame an anchor names that reads under no generation
    whole[anchors[1].start as usize + 60] ^= 1;
    fs::write(dir.path().join("log"), &whole).unwrap();
    let log = OpLog::open(dir.path()).unwrap();
    assert_eq!(log.state().unwrap().generation, u32::MAX);
    assert!(log.recent().is_empty());
    // every op is read instead, which finds the damage
    let opened = OpLog::open_at(dir.path(), Some(anchors[1]));
    assert!(matches!(
        opened,
        Err(OpLogError::Format("oplog record corrupt"))
    ));
    // a frame that does read is found under the generation of its block
    let log = OpLog::open_at(dir.path(), Some(anchors[3])).unwrap();
    assert!(log.anchored() && log.recent().is_empty());
}

#[test]
fn a_checkpoint_that_names_a_place_past_itself_is_not_begun_at() {
    let dir = alt_os::temp::tempdir().unwrap();
    varied(dir.path());
    let (whole, _) = written(dir.path());
    let (layout, frames) = laid(&whole);
    let written = OpLog::open(dir.path()).unwrap();
    assert!(written.bounded_to().unwrap() < written.appended_len());
    let last = frames
        .iter()
        .rev()
        .find(|frame| frame.head.kind == KIND_CHECKPOINT);
    let last = last.unwrap();
    let len = whole.len() as u64;
    for (field, set) in IN_CHECKPOINT.iter().enumerate().skip(2) {
        let mut said = Checkpoint::decode(&last.body).unwrap();
        set(&mut said, len + 1);
        let bytes = last.with(&layout, &last.head, &said.encode()).unwrap();
        let mut changed = whole.clone();
        changed[last.at as usize..last.end as usize].copy_from_slice(&bytes);
        fs::write(dir.path().join("log"), changed).unwrap();
        for in_boot in [2, 3] {
            boot(in_boot);
            // the checkpoint before it is, and the frames lead to the same log
            let log = OpLog::open(dir.path()).unwrap();
            assert_eq!(log.state(), written.state(), "field {field}");
            assert_eq!(log.bounded_to(), written.bounded_to(), "field {field}");
            assert_eq!(log.unsettled(), in_boot == 3, "field {field}");
        }
    }
}
