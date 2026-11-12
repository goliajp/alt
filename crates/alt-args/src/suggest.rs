//! Near-miss suggestions for mistyped names.

/// Jaro similarity of two strings, from 0 (nothing shared) to 1 (equal).
fn jaro(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() || b.is_empty() {
        return if a.is_empty() && b.is_empty() {
            1.0
        } else {
            0.0
        };
    }
    let range = (a.len().max(b.len()) / 2).saturating_sub(1);
    let mut a_matched = vec![false; a.len()];
    let mut b_matched = vec![false; b.len()];
    let mut matches = 0usize;
    for (i, a_char) in a.iter().enumerate() {
        let end = b.len().min(i + range + 1);
        for j in i.saturating_sub(range)..end {
            if !b_matched[j] && a_char == &b[j] {
                a_matched[i] = true;
                b_matched[j] = true;
                matches += 1;
                break;
            }
        }
    }
    if matches == 0 {
        return 0.0;
    }
    let a_shared = a.iter().zip(&a_matched).filter(|(_, m)| **m);
    let b_shared = b.iter().zip(&b_matched).filter(|(_, m)| **m);
    let transposed = a_shared.zip(b_shared).filter(|(x, y)| x.0 != y.0).count() / 2;
    let matches = matches as f64;
    (matches / a.len() as f64 + matches / b.len() as f64 + (matches - transposed as f64) / matches)
        / 3.0
}

/// The candidates close to `word`, the closest last.
pub(crate) fn similar<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    let mut found: Vec<(f64, &str)> = Vec::new();
    for candidate in candidates {
        let confidence = jaro(word, candidate);
        if confidence > 0.7 {
            let at = found.partition_point(|(other, _)| *other <= confidence);
            found.insert(at, (confidence, candidate));
        }
    }
    found.into_iter().map(|(_, candidate)| candidate).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaro_reference_values() {
        assert_eq!(jaro("", ""), 1.0);
        assert_eq!(jaro("a", ""), 0.0);
        assert_eq!(jaro("json", "json"), 1.0);
        assert!((jaro("martha", "marhta") - 0.944_444).abs() < 1e-5);
        assert!((jaro("dixon", "dicksonx") - 0.766_666).abs() < 1e-5);
        assert!((jaro("jellyfish", "smellyfish") - 0.896_296).abs() < 1e-5);
    }

    #[test]
    fn closest_candidate_is_last() {
        let names = ["rev-parse", "cat-file", "cherry-pick", "op-log", "status"];
        assert_eq!(similar("-", names), ["rev-parse", "cat-file", "op-log"]);
        assert_eq!(similar("jsno", ["pretty", "json", "patch"]), ["json"]);
        assert!(similar("nonesuch", names).is_empty());
    }
}
