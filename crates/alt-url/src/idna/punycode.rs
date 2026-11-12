const BASE: u32 = 36;
const T_MIN: u32 = 1;
const T_MAX: u32 = 26;
const SKEW: u32 = 38;
const DAMP: u32 = 700;
const INITIAL_BIAS: u32 = 72;
const INITIAL_N: u32 = 128;

fn adapt(delta: u32, count: u32, first: bool) -> u32 {
    let mut delta = if first { delta / DAMP } else { delta / 2 };
    delta += delta / count;
    let mut k = 0;
    while delta > (BASE - T_MIN) * T_MAX / 2 {
        delta /= BASE - T_MIN;
        k += BASE;
    }
    k + (BASE - T_MIN + 1) * delta / (delta + SKEW)
}

fn threshold(k: u32, bias: u32) -> u32 {
    k.saturating_sub(bias).clamp(T_MIN, T_MAX)
}

fn digit(c: char) -> Option<u32> {
    match c {
        'a'..='z' => Some(c as u32 - 'a' as u32),
        'A'..='Z' => Some(c as u32 - 'A' as u32),
        '0'..='9' => Some(c as u32 - '0' as u32 + 26),
        _ => None,
    }
}

/// Appends the code points that `input` encodes to `out`.
pub(super) fn decode(input: &[char], out: &mut Vec<char>) -> Option<()> {
    let start = out.len();
    let (basic, mut rest) = match input.iter().rposition(|&c| c == '-') {
        Some(delimiter) => (&input[..delimiter], &input[delimiter + 1..]),
        None => (&input[..0], input),
    };
    if !super::is_ascii(basic) {
        return None;
    }
    out.extend_from_slice(basic);
    let (mut n, mut i, mut bias) = (INITIAL_N, 0u32, INITIAL_BIAS);
    while !rest.is_empty() {
        let previous = i;
        let (mut weight, mut k) = (1u32, BASE);
        loop {
            let (&c, tail) = rest.split_first()?;
            rest = tail;
            let digit = digit(c)?;
            i = i.checked_add(digit.checked_mul(weight)?)?;
            let threshold = threshold(k, bias);
            if digit < threshold {
                break;
            }
            weight = weight.checked_mul(BASE - threshold)?;
            k += BASE;
        }
        let count = (out.len() - start) as u32 + 1;
        bias = adapt(i - previous, count, previous == 0);
        n = n.checked_add(i / count)?;
        i %= count;
        out.insert(start + i as usize, char::from_u32(n)?);
        i += 1;
    }
    Some(())
}

fn push_digit(out: &mut String, digit: u32) {
    out.push(
        (if digit < 26 {
            b'a' + digit as u8
        } else {
            b'0' + (digit - 26) as u8
        }) as char,
    );
}

/// Appends the encoding of `label`, without the `xn--` prefix, to `out`.
pub(super) fn encode(label: &[char], out: &mut String) -> Option<()> {
    let basic = label.iter().filter(|c| c.is_ascii()).count() as u32;
    out.extend(label.iter().filter(|c| c.is_ascii()));
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias, mut handled) = (INITIAL_N, 0u32, INITIAL_BIAS, basic);
    while (handled as usize) < label.len() {
        let next = label.iter().map(|&c| c as u32).filter(|&c| c >= n).min()?;
        delta = delta.checked_add((next - n).checked_mul(handled + 1)?)?;
        n = next;
        for &c in label {
            let c = c as u32;
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c != n {
                continue;
            }
            let (mut q, mut k) = (delta, BASE);
            loop {
                let threshold = threshold(k, bias);
                if q < threshold {
                    break;
                }
                push_digit(out, threshold + (q - threshold) % (BASE - threshold));
                q = (q - threshold) / (BASE - threshold);
                k += BASE;
            }
            push_digit(out, q);
            bias = adapt(delta, handled + 1, handled == basic);
            delta = 0;
            handled += 1;
        }
        delta += 1;
        n += 1;
    }
    Some(())
}
