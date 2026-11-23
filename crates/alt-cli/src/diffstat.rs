//! The summary `--stat` prints: for each file its name, how many lines
//! changed and a bar of that length, then the totals. Laid out as git
//! lays it out, in 80 columns.

/// One file of the summary.
pub(crate) struct FileStat {
    /// the name as it is shown, a rename as `old => new`
    pub(crate) name: Vec<u8>,
    pub(crate) change: Change,
}

pub(crate) enum Change {
    /// lines added and removed
    Lines(usize, usize),
    /// a binary file: its size before and after
    Binary(usize, usize),
}

const WIDTH: usize = 80;

fn digits(mut number: usize) -> usize {
    let mut width = 1;
    while number >= 10 {
        number /= 10;
        width += 1;
    }
    width
}

/// A bar of `it` out of `max` changes in `width` columns: at least one
/// column for anything that changed.
fn scaled(it: usize, width: usize, max: usize) -> usize {
    match it {
        0 => 0,
        _ => 1 + it * (width - 1) / max,
    }
}

/// How a rename is shown: the part of the two paths that differs in
/// braces, between what they begin and end with alike.
pub(crate) fn renamed(old: &[u8], new: &[u8]) -> Vec<u8> {
    let common = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    // what they begin with alike ends in a slash
    let prefix = old[..common]
        .iter()
        .rposition(|b| *b == b'/')
        .map_or(0, |at| at + 1);
    // what they end with alike begins with one, which may be that slash
    let reach = prefix.saturating_sub(1);
    let mut suffix = 0;
    let (mut a, mut b) = (old.len(), new.len());
    while a > reach && b > reach && old[a - 1] == new[b - 1] {
        (a, b) = (a - 1, b - 1);
        if old[a] == b'/' {
            suffix = old.len() - a;
        }
    }
    let middle = |path: &[u8]| path[prefix..path.len().saturating_sub(suffix).max(prefix)].to_vec();
    let mut shown = Vec::new();
    if prefix + suffix > 0 {
        shown.extend_from_slice(&old[..prefix]);
        shown.push(b'{');
    }
    shown.extend(middle(old));
    shown.extend_from_slice(b" => ");
    shown.extend(middle(new));
    if prefix + suffix > 0 {
        shown.push(b'}');
        shown.extend_from_slice(&old[old.len() - suffix..]);
    }
    shown
}

/// How wide each part of a line is.
struct Widths {
    name: usize,
    number: usize,
    graph: usize,
    /// the most lines one file changed
    max_change: usize,
}

fn changed(file: &FileStat) -> usize {
    match file.change {
        Change::Lines(added, removed) => added + removed,
        Change::Binary(..) => 0,
    }
}

impl Widths {
    /// git's division of the columns among the names, the counts and the
    /// bars: a name gets what it needs where the bars leave it, and bars
    /// that do not fit are scaled down to what is left.
    fn of(files: &[FileStat]) -> Widths {
        let max_change = files.iter().map(changed).max().unwrap_or(0);
        let max_name = files.iter().map(|file| file.name.len()).max().unwrap_or(0);
        // "Bin 1 -> 22 bytes" where a binary file changed size
        let bin = |file: &FileStat| match file.change {
            Change::Binary(0, 0) | Change::Lines(..) => 0,
            Change::Binary(old, new) => 14 + digits(old) + digits(new),
        };
        let bin_width = files.iter().map(bin).max().unwrap_or(0);
        let mut number = digits(max_change);
        if bin_width > 0 && number < 3 {
            number = 3;
        }
        let width = WIDTH.max(16 + 6 + number);
        let mut graph = match max_change + 4 > bin_width {
            true => max_change,
            false => bin_width - 4,
        };
        let mut name = max_name;
        if name + number + 6 + graph > width {
            if graph > width * 3 / 8 - number - 6 {
                graph = (width * 3 / 8 - number - 6).max(6);
            }
            if name > width - number - 6 - graph {
                name = width - number - 6 - graph;
            } else {
                graph = width - number - 6 - name;
            }
        }
        Widths {
            name,
            number,
            graph,
            max_change,
        }
    }

    /// The count of a file and its bar: `added` pluses and `removed`
    /// minuses, scaled where the longest bar does not fit.
    fn bar(&self, added: usize, removed: usize) -> String {
        let (mut plus, mut minus) = (added, removed);
        if self.graph <= self.max_change {
            let mut total = scaled(added + removed, self.graph, self.max_change);
            if total < 2 && added > 0 && removed > 0 {
                total = 2;
            }
            if added < removed {
                plus = scaled(added, self.graph, self.max_change);
                minus = total - plus;
            } else {
                minus = scaled(removed, self.graph, self.max_change);
                plus = total - minus;
            }
        }
        let count = added + removed;
        let gap = if count > 0 { " " } else { "" };
        let number = self.number;
        format!(
            " {count:>number$}{gap}{}{}",
            "+".repeat(plus),
            "-".repeat(minus)
        )
    }
}

/// The summary of `files`, its last line the totals; nothing where there
/// are no files.
pub(crate) fn render(files: &[FileStat]) -> Vec<u8> {
    let mut out = Vec::new();
    if files.is_empty() {
        return out;
    }
    let widths = Widths::of(files);
    let number = widths.number;
    let (mut inserted, mut deleted) = (0, 0);
    for file in files {
        // a name too long loses its beginning, up to a slash where there is one
        let mut name = &file.name[..];
        let mut cut = "";
        if name.len() > widths.name {
            name = &name[name.len() - widths.name + 3..];
            if let Some(slash) = name.iter().position(|b| *b == b'/') {
                name = &name[slash..];
            }
            cut = "...";
        }
        out.extend_from_slice(format!(" {cut}").as_bytes());
        out.extend_from_slice(name);
        let padding = widths.name.saturating_sub(cut.len() + name.len());
        out.extend(std::iter::repeat_n(b' ', padding));
        out.extend_from_slice(b" |");
        let told = match file.change {
            Change::Binary(0, 0) => format!(" {:>number$}", "Bin"),
            Change::Binary(old, new) => format!(" {:>number$} {old} -> {new} bytes", "Bin"),
            Change::Lines(added, removed) => {
                (inserted, deleted) = (inserted + added, deleted + removed);
                widths.bar(added, removed)
            }
        };
        out.extend_from_slice(told.as_bytes());
        out.push(b'\n');
    }
    let plural = |count: usize| if count == 1 { "" } else { "s" };
    let count = files.len();
    let mut totals = format!(" {count} file{} changed", plural(count));
    if inserted > 0 || deleted == 0 {
        totals += &format!(", {inserted} insertion{}(+)", plural(inserted));
    }
    if deleted > 0 || inserted == 0 {
        totals += &format!(", {deleted} deletion{}(-)", plural(deleted));
    }
    out.extend_from_slice(totals.as_bytes());
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(name: &str, added: usize, removed: usize) -> FileStat {
        FileStat {
            name: name.as_bytes().to_vec(),
            change: Change::Lines(added, removed),
        }
    }

    fn shown(files: &[FileStat]) -> String {
        String::from_utf8(render(files)).unwrap()
    }

    #[test]
    fn names_are_padded_and_counts_lined_up() {
        let said = shown(&[lines("a", 1, 1), lines("d/n", 1, 0), lines("gone", 0, 12)]);
        let expected = " a    |  2 +-\n d/n  |  1 +\n gone | 12 ------------\n \
                        3 files changed, 2 insertions(+), 13 deletions(-)\n";
        assert_eq!(said, expected);
        assert_eq!(shown(&[]), "");
        let said = shown(&[lines("mode", 0, 0)]);
        assert_eq!(
            said,
            " mode | 0\n 1 file changed, 0 insertions(+), 0 deletions(-)\n"
        );
        let said = shown(&[lines("one", 1, 0)]);
        assert_eq!(said, " one | 1 +\n 1 file changed, 1 insertion(+)\n");
    }

    #[test]
    fn a_long_bar_is_scaled_to_the_columns_there_are() {
        let said = shown(&[lines("big", 300, 100), lines("small", 1, 1)]);
        let first = said.lines().next().unwrap();
        assert_eq!(first.len(), 79, "{said}");
        assert!(first.starts_with(" big   | 400 +++++"), "{said}");
        assert_eq!(said.lines().nth(1).unwrap(), " small |   2 +-");
    }

    #[test]
    fn a_binary_file_is_told_by_its_sizes() {
        let binary = FileStat {
            name: b"pic".to_vec(),
            change: Change::Binary(0, 10),
        };
        let said = shown(&[binary, lines("t", 1, 0)]);
        let expected = " pic | Bin 0 -> 10 bytes\n t   |   1 +\n 2 files changed, 1 insertion(+)\n";
        assert_eq!(said, expected);
    }

    #[test]
    fn a_rename_shows_what_differs_in_braces() {
        let show =
            |old: &str, new: &str| String::from_utf8(renamed(old.as_bytes(), new.as_bytes()));
        assert_eq!(show("a", "b").unwrap(), "a => b");
        assert_eq!(show("d/a", "d/b").unwrap(), "d/{a => b}");
        assert_eq!(show("a/x", "b/x").unwrap(), "{a => b}/x");
        assert_eq!(show("p/a/x", "p/b/x").unwrap(), "p/{a => b}/x");
        assert_eq!(show("p/x", "p/q/x").unwrap(), "p/{ => q}/x");
        assert_eq!(show("x", "d/x").unwrap(), "x => d/x");
    }
}
