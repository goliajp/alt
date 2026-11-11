/// "00" through "99", two bytes each.
pub(crate) const PAIRS: [u8; 200] = {
    let mut table = [0u8; 200];
    let mut i = 0;
    while i < 100 {
        table[2 * i] = b'0' + (i / 10) as u8;
        table[2 * i + 1] = b'0' + (i % 10) as u8;
        i += 1;
    }
    table
};

pub(crate) fn write_u64(mut n: u64, out: &mut Vec<u8>) {
    let mut buf = [0u8; 20];
    let mut at = buf.len();
    while n >= 100 {
        let pair = (n % 100) as usize * 2;
        n /= 100;
        at -= 2;
        buf[at..at + 2].copy_from_slice(&PAIRS[pair..pair + 2]);
    }
    if n >= 10 {
        let pair = n as usize * 2;
        at -= 2;
        buf[at..at + 2].copy_from_slice(&PAIRS[pair..pair + 2]);
    } else {
        at -= 1;
        buf[at] = b'0' + n as u8;
    }
    out.extend_from_slice(&buf[at..]);
}

pub(crate) fn write_i64(n: i64, out: &mut Vec<u8>) {
    if n < 0 {
        out.push(b'-');
    }
    write_u64(n.unsigned_abs(), out);
}
