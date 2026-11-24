//! What the first commit of the task fixture holds.
use std::path::Path;

/// The source the task changes, the gate that is run on it, and more files in
/// one directory than a page of it shows.
pub(super) fn write(directory: &Path) {
    std::fs::write(directory.join("source.txt"), "base\n").unwrap();
    std::fs::create_dir(directory.join("wide")).unwrap();
    for file in 0..600 {
        std::fs::write(directory.join(format!("wide/f{file:04}")), "wide\n").unwrap();
    }
    let gate = "test -s source.txt\nprintf verified\n";
    std::fs::write(directory.join("gate.sh"), gate).unwrap();
}
