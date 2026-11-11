//! A minimal frame walker that reports which block kinds and entropy modes
//! a frame uses, so the tests can tell that the corpus reaches all of them.

#[derive(Default, Debug)]
pub struct Shape {
    /// raw, single-byte, compressed
    pub blocks: [usize; 3],
    /// raw, single-byte, compressed, reusing the previous tree
    pub literals: [usize; 4],
    pub one_stream: usize,
    pub four_streams: usize,
    /// predefined, single-symbol, described, repeated; for each of
    /// literal length, offset, match length
    pub modes: [[usize; 4]; 3],
    pub without_sequences: usize,
}

fn le(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rev()
        .fold(0, |value, &byte| value << 8 | byte as usize)
}

/// Adds the shape of one frame, which must be well formed and stand alone.
pub fn add(frame: &[u8], shape: &mut Shape) {
    assert_eq!(&frame[..4], [0x28, 0xB5, 0x2F, 0xFD]);
    let descriptor = frame[4];
    let single = descriptor & 0x20 != 0;
    let content = match descriptor >> 6 {
        0 => single as usize,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let dictionary = [0, 1, 2, 4][(descriptor & 3) as usize];
    let mut at = 5 + !single as usize + dictionary + content;
    loop {
        let header = le(&frame[at..at + 3]);
        let (last, kind, len) = (header & 1, (header >> 1) & 3, header >> 3);
        at += 3;
        shape.blocks[kind] += 1;
        match kind {
            0 => at += len,
            1 => at += 1,
            _ => {
                block(&frame[at..at + len], shape);
                at += len;
            }
        }
        if last == 1 {
            break;
        }
    }
}

fn block(body: &[u8], shape: &mut Shape) {
    let kind = (body[0] & 3) as usize;
    let format = (body[0] >> 2) & 3;
    shape.literals[kind] += 1;
    let section = if kind < 2 {
        let (header, len) = match format {
            1 => (2, le(&body[..2]) >> 4),
            3 => (3, le(&body[..3]) >> 4),
            _ => (1, (body[0] >> 3) as usize),
        };
        header + if kind == 0 { len } else { 1 }
    } else {
        if format == 0 {
            shape.one_stream += 1;
        } else {
            shape.four_streams += 1;
        }
        match format {
            0 | 1 => 3 + (le(&body[..3]) >> 14),
            2 => 4 + (le(&body[..4]) >> 18),
            _ => 5 + (le(&body[..5]) >> 22),
        }
    };
    let rest = &body[section..];
    let (count, header) = match rest[0] {
        0..=127 => (rest[0] as usize, 1),
        128..=254 => (((rest[0] as usize - 128) << 8) + rest[1] as usize, 2),
        _ => (le(&rest[1..3]) + 0x7F00, 3),
    };
    if count == 0 {
        shape.without_sequences += 1;
        return;
    }
    let modes = rest[header];
    for field in 0..3 {
        shape.modes[field][((modes >> (6 - 2 * field)) & 3) as usize] += 1;
    }
}
