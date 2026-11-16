//! The paths a walk is limited to, however many they are.

/// How a path stands to the paths a walk is limited to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reach {
    /// one of them, or beneath one
    Inside,
    /// a directory above one
    Above,
}

/// The paths a walk looks at, each with everything beneath it, in order.
pub(crate) struct Within<'a>(Vec<&'a [u8]>);

/// Whether `path` comes before `dir` with a `/` at its end.
fn before_inside(path: &[u8], dir: &[u8]) -> bool {
    match path.len() > dir.len() && path.starts_with(dir) {
        true => path[dir.len()] < b'/',
        false => path <= dir,
    }
}

impl<'a> Within<'a> {
    pub(crate) fn new(paths: &[&'a [u8]]) -> Within<'a> {
        let mut paths = paths.to_vec();
        if !paths.is_sorted() {
            paths.sort_unstable();
        }
        Within(paths)
    }

    fn has(&self, path: &[u8]) -> bool {
        self.0.binary_search(&path).is_ok()
    }

    /// Whether `path` is one of the paths or beneath one.
    pub(crate) fn holds(&self, path: &[u8]) -> bool {
        self.has(path) || (0..path.len()).any(|at| path[at] == b'/' && self.has(&path[..at]))
    }

    /// Whether one of the paths lies beneath `dir`.
    fn reaches_into(&self, dir: &[u8]) -> bool {
        // those beneath `dir` stand together, where `dir/` would
        let first = self.0.partition_point(|path| before_inside(path, dir));
        self.0.get(first).is_some_and(|path| {
            path.len() > dir.len() && path[dir.len()] == b'/' && path.starts_with(dir)
        })
    }

    /// How `path` stands to the paths, in a directory that stands to them
    /// as `parent` says; `None` when a walk has nothing to look at there.
    pub(crate) fn reach(&self, parent: Reach, path: &[u8]) -> Option<Reach> {
        if parent == Reach::Inside || self.holds(path) {
            return Some(Reach::Inside);
        }
        self.reaches_into(path).then_some(Reach::Above)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `reach` answered before the paths were kept in order.
    fn one_by_one(within: &[&[u8]], path: &[u8]) -> Option<Reach> {
        let beneath = |path: &[u8], dir: &[u8]| {
            path.len() > dir.len() && path[dir.len()] == b'/' && path.starts_with(dir)
        };
        if within.iter().any(|w| *w == path || beneath(path, w)) {
            return Some(Reach::Inside);
        }
        within
            .iter()
            .any(|w| beneath(w, path))
            .then_some(Reach::Above)
    }

    #[test]
    fn many_paths_in_order_reach_what_each_of_them_reaches() {
        // names that sort between a directory and what lies in it
        let names: [&[u8]; 6] = [b"a", b"a-b", b"a.b", b"a0", b"ab", b"b"];
        let mut all: Vec<Vec<u8>> = Vec::new();
        for one in names {
            all.push(one.to_vec());
            for two in names {
                all.push([one, b"/", two].concat());
                for three in names {
                    all.push([one, b"/", two, b"/", three].concat());
                }
            }
        }
        // every seventh, then every fifth from another start: some lie
        // beneath others, some are given twice
        for (step, from) in [(7, 0), (5, 3), (64, 1), (1, 0)] {
            let mut chosen: Vec<&[u8]> = all
                .iter()
                .skip(from)
                .step_by(step)
                .map(Vec::as_slice)
                .collect();
            chosen.extend(all.iter().skip(from).step_by(step * 3).map(Vec::as_slice));
            chosen.reverse();
            let within = Within::new(&chosen);
            for path in &all {
                let got = within.reach(Reach::Above, path);
                assert!(
                    got == one_by_one(&chosen, path),
                    "{:?} among every {step}th",
                    String::from_utf8_lossy(path)
                );
                assert!(within.reach(Reach::Inside, path) == Some(Reach::Inside));
            }
        }
        assert!(Within::new(&[]).reach(Reach::Above, b"a").is_none());
    }
}
