//! Rename detection between the files a change removes and the files it
//! adds, pairing them the way git's default `diff` does: identical content
//! first, then files that kept their name, then the best remaining matches
//! by similarity.

use std::collections::HashMap;

/// Similarity of two identical files; scores are fractions of this.
pub const MAX_SCORE: u32 = 60000;
/// A pair below half similarity is not a rename.
const MIN_SCORE: u32 = 30000;
/// A pair matched on file name alone needs to be this similar.
const MIN_NAME_SCORE: u32 = 45000;
/// With more removed-added combinations than the square of this, only
/// identical content and unchanged names are paired.
const LIMIT: u64 = 1000;
const CANDIDATES: usize = 4;

/// A removed file that an added file may have been renamed from.
#[derive(Clone, Copy)]
struct Candidate {
    score: u32,
    /// whether the two share a file name
    named: bool,
    src: usize,
    dst: usize,
}

/// Better candidates first: by score, then a shared file name. An empty
/// slot comes after any candidate.
fn order(a: &Option<Candidate>, b: &Option<Candidate>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(a), Some(b)) => (b.score, b.named).cmp(&(a.score, a.named)),
    }
}

/// A removed or added file.
#[derive(Clone, Copy)]
pub struct File<'a> {
    pub path: &'a [u8],
    /// content identity: equal ids mean equal content
    pub id: &'a [u8],
    pub mode: u32,
}

/// `removed[src]` was renamed to `added[dst]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rename {
    pub src: usize,
    pub dst: usize,
    /// similarity out of [`MAX_SCORE`]
    pub score: u32,
}

impl Rename {
    /// The percentage git prints as `similarity index`.
    pub fn percent(&self) -> u32 {
        self.score * 100 / MAX_SCORE
    }
}

fn regular(mode: u32) -> bool {
    mode & 0o170000 == 0o100000
}

fn basename(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}

/// Pairs removed files with added files. `content` loads a file's bytes:
/// `(true, i)` for `removed[i]`, `(false, i)` for `added[i]`.
pub fn detect<E>(
    removed: &[File<'_>],
    added: &[File<'_>],
    content: &mut dyn FnMut(bool, usize) -> Result<Vec<u8>, E>,
) -> Result<Vec<Rename>, E> {
    let mut state = Pairing {
        removed,
        added,
        src_used: vec![false; removed.len()],
        dst_used: vec![false; added.len()],
        renames: Vec::new(),
        spans: HashMap::new(),
    };
    state.identical();
    if state.src_used.iter().all(|u| *u) || state.dst_used.iter().all(|u| *u) {
        return Ok(state.finish());
    }
    state.same_name(content)?;
    state.most_similar(content)?;
    Ok(state.finish())
}

/// A file's size, and the bytes under each of its span hashes.
type Spans = (u64, Vec<(u32, u64)>);

struct Pairing<'a> {
    removed: &'a [File<'a>],
    added: &'a [File<'a>],
    src_used: Vec<bool>,
    dst_used: Vec<bool>,
    renames: Vec<Rename>,
    /// the files read so far, by side and position
    spans: HashMap<(bool, usize), Spans>,
}

impl Pairing<'_> {
    fn finish(mut self) -> Vec<Rename> {
        self.renames.sort_by_key(|r| r.dst);
        self.renames
    }

    fn pair(&mut self, src: usize, dst: usize, score: u32) {
        self.src_used[src] = true;
        self.dst_used[dst] = true;
        self.renames.push(Rename { src, dst, score });
    }

    /// Files whose content did not change at all. Among several removed
    /// files with the content, one that kept its name is preferred.
    fn identical(&mut self) {
        let mut by_id: HashMap<&[u8], Vec<usize>> = HashMap::new();
        for (i, f) in self.removed.iter().enumerate() {
            by_id.entry(f.id).or_default().push(i);
        }
        for (dst, target) in self.added.iter().enumerate() {
            let Some(sources) = by_id.get(target.id) else {
                continue;
            };
            let mut best: Option<(usize, bool)> = None;
            for &src in sources {
                let source = &self.removed[src];
                // anything but a regular file has to keep its kind
                let kinds_agree =
                    (regular(source.mode) && regular(target.mode)) || source.mode == target.mode;
                if self.src_used[src] || !kinds_agree {
                    continue;
                }
                let same_name = basename(source.path) == basename(target.path);
                if best.is_none_or(|(_, named)| same_name && !named) {
                    best = Some((src, same_name));
                }
                if same_name {
                    break;
                }
            }
            if let Some((src, _)) = best {
                self.pair(src, dst, MAX_SCORE);
            }
        }
    }

    /// A removed file and an added file that are each the only one left
    /// with their file name are compared before anything else: most
    /// renames move a file to another directory under the same name.
    fn same_name<E>(
        &mut self,
        content: &mut dyn FnMut(bool, usize) -> Result<Vec<u8>, E>,
    ) -> Result<(), E> {
        let unique = |files: &[File<'_>], used: &[bool]| {
            let mut names: HashMap<Vec<u8>, Option<usize>> = HashMap::new();
            for (i, f) in files.iter().enumerate() {
                if !used[i] {
                    names
                        .entry(basename(f.path).to_vec())
                        .and_modify(|slot| *slot = None)
                        .or_insert(Some(i));
                }
            }
            names
        };
        let sources = unique(self.removed, &self.src_used);
        let targets = unique(self.added, &self.dst_used);
        for src in 0..self.removed.len() {
            if self.src_used[src] {
                continue;
            }
            let name = basename(self.removed[src].path);
            let (Some(Some(only)), Some(Some(dst))) = (sources.get(name), targets.get(name)) else {
                continue;
            };
            debug_assert_eq!(*only, src);
            if self.dst_used[*dst] {
                continue;
            }
            let score = self.similarity(src, *dst, MIN_NAME_SCORE, content)?;
            if score >= MIN_NAME_SCORE {
                self.pair(src, *dst, score);
            }
        }
        Ok(())
    }

    /// Every remaining added file against every remaining removed file:
    /// the best few candidates of each added file are kept, and pairs are
    /// then made from the most similar down.
    fn most_similar<E>(
        &mut self,
        content: &mut dyn FnMut(bool, usize) -> Result<Vec<u8>, E>,
    ) -> Result<(), E> {
        let sources: Vec<usize> = (0..self.removed.len())
            .filter(|&i| !self.src_used[i])
            .collect();
        let targets: Vec<usize> = (0..self.added.len())
            .filter(|&i| !self.dst_used[i])
            .collect();
        if sources.len() as u64 * targets.len() as u64 > LIMIT * LIMIT {
            return Ok(());
        }
        // each added file keeps its best few candidates in a fixed set of
        // slots, a better one taking the place of the worst; where equally
        // good candidates end up depends on that order, and git's choice
        // among them with it
        let mut slots: Vec<Option<Candidate>> = Vec::new();
        for &dst in &targets {
            let mut best = [None; CANDIDATES];
            for &src in &sources {
                let candidate = Candidate {
                    score: self.similarity(src, dst, MIN_SCORE, content)?,
                    named: basename(self.removed[src].path) == basename(self.added[dst].path),
                    src,
                    dst,
                };
                let mut worst = 0;
                for i in 1..CANDIDATES {
                    if order(&best[i], &best[worst]).is_gt() {
                        worst = i;
                    }
                }
                if order(&best[worst], &Some(candidate)).is_gt() {
                    best[worst] = Some(candidate);
                }
            }
            slots.extend(best);
        }
        slots.sort_by(order);
        for candidate in slots.into_iter().flatten() {
            if candidate.score < MIN_SCORE {
                break;
            }
            if !self.src_used[candidate.src] && !self.dst_used[candidate.dst] {
                self.pair(candidate.src, candidate.dst, candidate.score);
            }
        }
        Ok(())
    }

    /// How much of the larger of two regular files is made of pieces of
    /// the removed one, out of [`MAX_SCORE`]; zero for a pair that cannot
    /// reach `minimum`.
    fn similarity<E>(
        &mut self,
        src: usize,
        dst: usize,
        minimum: u32,
        content: &mut dyn FnMut(bool, usize) -> Result<Vec<u8>, E>,
    ) -> Result<u32, E> {
        if !regular(self.removed[src].mode) || !regular(self.added[dst].mode) {
            return Ok(0);
        }
        for key in [(true, src), (false, dst)] {
            if let std::collections::hash_map::Entry::Vacant(slot) = self.spans.entry(key) {
                let bytes = content(key.0, key.1)?;
                slot.insert((bytes.len() as u64, spans(&bytes)));
            }
        }
        let (src_size, src_spans) = &self.spans[&(true, src)];
        let (dst_size, dst_spans) = &self.spans[&(false, dst)];
        let (max, min) = (*src_size.max(dst_size), *src_size.min(dst_size));
        // sizes too far apart to share enough
        if max * u64::from(MAX_SCORE - minimum) < (max - min) * u64::from(MAX_SCORE) {
            return Ok(0);
        }
        if *dst_size == 0 {
            return Ok(0);
        }
        Ok((copied(src_spans, dst_spans) * u64::from(MAX_SCORE) / max) as u32)
    }
}

/// Cuts content into spans, at each newline and at 64 bytes, and counts
/// the bytes under each span hash, sorted by hash. In text, the CR of a
/// CRLF is skipped.
fn spans(data: &[u8]) -> Vec<(u32, u64)> {
    const BASE: u32 = 107927;
    let text = !crate::is_binary(data);
    let mut counts: HashMap<u32, u64> = HashMap::new();
    let (mut a, mut b, mut n) = (0u32, 0u32, 0u64);
    for (i, &c) in data.iter().enumerate() {
        if text && c == b'\r' && data.get(i + 1) == Some(&b'\n') {
            continue;
        }
        let old_a = a;
        a = (a << 7) ^ (b >> 25);
        b = (b << 7) ^ (old_a >> 25);
        a = a.wrapping_add(u32::from(c));
        n += 1;
        if n < 64 && c != b'\n' {
            continue;
        }
        *counts
            .entry(a.wrapping_add(b.wrapping_mul(0x61)) % BASE)
            .or_default() += n;
        (a, b, n) = (0, 0, 0);
    }
    if n > 0 {
        *counts
            .entry(a.wrapping_add(b.wrapping_mul(0x61)) % BASE)
            .or_default() += n;
    }
    let mut sorted: Vec<(u32, u64)> = counts.into_iter().collect();
    sorted.sort_unstable();
    sorted
}

/// Bytes of the source that also occur in the target, span for span.
fn copied(src: &[(u32, u64)], dst: &[(u32, u64)]) -> u64 {
    let mut total = 0;
    let mut d = 0;
    for &(hash, count) in src {
        while d < dst.len() && dst[d].0 < hash {
            d += 1;
        }
        if d < dst.len() && dst[d].0 == hash {
            total += count.min(dst[d].1);
            d += 1;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file<'a>((path, text): &(&'a str, &'a str)) -> File<'a> {
        File {
            path: path.as_bytes(),
            id: text.as_bytes(),
            mode: 0o100644,
        }
    }

    fn run(removed: &[(&str, &str)], added: &[(&str, &str)]) -> Vec<(usize, usize, u32)> {
        let r: Vec<File<'_>> = removed.iter().map(file).collect();
        let a: Vec<File<'_>> = added.iter().map(file).collect();
        let mut load = |src: bool, i: usize| -> Result<Vec<u8>, ()> {
            Ok(if src { removed[i].1 } else { added[i].1 }
                .as_bytes()
                .to_vec())
        };
        detect(&r, &a, &mut load)
            .unwrap()
            .iter()
            .map(|p| (p.src, p.dst, p.percent()))
            .collect()
    }

    const BODY: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

    #[test]
    fn identical_content_pairs_at_full_score_and_prefers_the_same_name() {
        let removed = [("a/x.txt", BODY), ("b/keep.txt", BODY)];
        let added = [("c/keep.txt", BODY)];
        assert_eq!(run(&removed, &added), vec![(1, 0, 100)]);
    }

    #[test]
    fn a_lightly_edited_file_is_a_rename_and_a_rewritten_one_is_not() {
        let edited = BODY.replace("five", "FIVE");
        assert_eq!(
            run(&[("old.txt", BODY)], &[("new.txt", &edited)]),
            vec![(0, 0, 89)]
        );
        let other = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\niota\nkappa\n";
        assert_eq!(run(&[("old.txt", BODY)], &[("new.txt", other)]), vec![]);
    }

    #[test]
    fn each_removed_file_is_used_once_and_the_most_similar_wins() {
        let close = BODY.replace("ten", "TEN");
        let far = BODY.replace("one\ntwo\nthree\n", "1\n2\n3\n");
        // both added files resemble the one removed file; the closer one
        // takes it and the other stays an addition
        assert_eq!(
            run(
                &[("src.txt", BODY)],
                &[("far.txt", &far), ("near.txt", &close)]
            )
            .iter()
            .map(|p| (p.0, p.1))
            .collect::<Vec<_>>(),
            vec![(0, 1)]
        );
    }

    #[test]
    fn equally_similar_files_are_chosen_between_as_git_does() {
        // ten lines of equal length; a source sharing n of them scores n/10
        let lines = |shared: usize, tag: &str| -> String {
            (0..10)
                .map(|i| match i < 10 - shared {
                    true => format!("{tag}-{i:02}\n"),
                    false => format!("same-{i:02}\n"),
                })
                .collect()
        };
        let target = lines(10, "");
        let sources = [
            ("a", lines(4, "aaaa")),
            ("b", lines(6, "bbbb")),
            ("c", lines(5, "cccc")),
            ("d", lines(0, "dddd")),
            ("g", lines(9, "gggg")),
            ("s", lines(9, "ssss")),
        ];
        let removed: Vec<(&str, &str)> = sources.iter().map(|(p, t)| (*p, t.as_str())).collect();
        // `g` and `s` are equally similar. The weaker files seen before
        // them decide which slot each lands in: `g` displaces the worst,
        // `d`, in the last slot, `s` then displaces `a` in the first, and
        // so comes first among equals
        assert_eq!(run(&removed, &[("t", &target)]), vec![(5, 0, 90)]);
        // without the weaker files in front, the earlier of the two wins
        assert_eq!(run(&removed[4..], &[("t", &target)]), vec![(0, 0, 90)]);
    }

    #[test]
    fn symlinks_pair_only_when_identical() {
        let link = |path: &'static str, target: &'static str| File {
            path: path.as_bytes(),
            id: target.as_bytes(),
            mode: 0o120000,
        };
        let mut load = |_: bool, _: usize| -> Result<Vec<u8>, ()> { Ok(b"target".to_vec()) };
        let same = detect(&[link("a", "target")], &[link("b", "target")], &mut load).unwrap();
        assert_eq!(same.len(), 1);
        let differ = detect(&[link("a", "target")], &[link("b", "other")], &mut load).unwrap();
        assert!(differ.is_empty());
        // a link never pairs with a regular file of the same content
        let file = File {
            path: b"b",
            id: b"target",
            mode: 0o100644,
        };
        assert!(
            detect(&[link("a", "target")], &[file], &mut load)
                .unwrap()
                .is_empty()
        );
    }
}
