//! What the refs of a repository were found to lead to, on the disk of the
//! stored bytes, so that a server that starts again goes on from it instead of
//! from nothing.
//!
//! It is computed from the segments and the refs and is nothing beside them:
//! one file a repository, written in the place of the one before. Wherever it
//! is not there, is not read as written, or names tips the refs of now do not
//! lead to, what the refs lead to is found anew and written over it.

use alt_pack_segment::{Led, Oid};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 17] = b"alt-led-computed1";
const DIGEST: usize = 32;

fn directory(root: &Path) -> PathBuf {
    root.join("packs").join("led_computed")
}

fn path(root: &Path, repository: &str) -> PathBuf {
    let name = &alt_hash::blake3::hash(repository.as_bytes()).to_hex()[..32];
    directory(root).join(format!("{name}.led"))
}

/// Removes the files beside those of the repositories that a process which
/// ended before it renamed them left, and returns how many there were: what
/// another writes in the meantime is not written, and found anew later.
pub fn left_behind_removed(root: &Path) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(directory(root)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        entries => entries?,
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let beside = name.to_str().and_then(|name| name.split_once(".led."));
        if beside.is_some_and(|(_, pid)| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// what [`written`] wrote for `repository`, where it is that
pub(super) fn read(root: &Path, repository: &str) -> Option<(Vec<Oid>, Led)> {
    let bytes = std::fs::read(path(root, repository)).ok()?;
    let (body, digest) = bytes.split_at_checked(bytes.len().checked_sub(DIGEST)?)?;
    if alt_hash::blake3::hash(body).as_bytes() != digest {
        return None;
    }
    let mut rest = body.strip_prefix(MAGIC.as_slice())?;
    let mut take = |n: usize| {
        let (taken, after) = rest.split_at_checked(n)?;
        rest = after;
        Some(taken)
    };
    let number = |bytes: &[u8]| u64::from_le_bytes(bytes.try_into().expect("eight bytes"));
    let named = number(take(8)?);
    if take(usize::try_from(named).ok()?)? != repository.as_bytes() {
        return None;
    }
    let tips = usize::try_from(number(take(8)?)).ok()?;
    let objects = usize::try_from(number(take(8)?)).ok()?;
    let ids = |n: usize, bytes: &[u8]| -> Vec<Oid> {
        let (ids, _) = bytes.as_chunks::<20>();
        ids.iter().take(n).copied().collect()
    };
    let tips = ids(tips, take(tips.checked_mul(20)?)?);
    let objects = ids(objects, take(objects.checked_mul(20)?)?);
    if !rest.is_empty() || !tips.is_sorted() {
        return None;
    }
    Some((tips, Led::again(objects)?))
}

/// Writes what `tips` lead to in the place of what was written for the
/// repository before: a file beside it that is renamed into its place, so that
/// a reader finds the one or the other whole.
pub(super) fn written(
    root: &Path,
    repository: &str,
    tips: &[Oid],
    led: &Led,
) -> std::io::Result<()> {
    let target = path(root, repository);
    let directory = target.parent().expect("a directory");
    std::fs::create_dir_all(directory)?;
    let mut bytes = Vec::with_capacity(
        MAGIC.len() + repository.len() + 24 + (tips.len() + led.len()) * 20 + DIGEST,
    );
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(repository.len() as u64).to_le_bytes());
    bytes.extend_from_slice(repository.as_bytes());
    bytes.extend_from_slice(&(tips.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&(led.len() as u64).to_le_bytes());
    tips.iter()
        .chain(led.objects())
        .for_each(|id| bytes.extend_from_slice(id));
    let digest = alt_hash::blake3::hash(&bytes);
    bytes.extend_from_slice(digest.as_bytes());
    let beside = target.with_extension(format!("led.{}", std::process::id()));
    let written = std::fs::File::create(&beside)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| std::fs::rename(&beside, &target));
    if written.is_err() {
        let _ = std::fs::remove_file(&beside);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_written_is_read_and_nothing_else_is() {
        let root = alt_os::temp::tempdir().unwrap();
        let root = root.path();
        let tips = vec![[1; 20], [7; 20]];
        let led = Led::again(vec![[9; 20], [1; 20], [7; 20], [3; 20]]).unwrap();
        written(root, "one", &tips, &led).unwrap();
        let (read_tips, read_led) = read(root, "one").unwrap();
        assert_eq!(read_tips, tips);
        let objects = |led: &Led| {
            let mut all: Vec<Oid> = led.objects().copied().collect();
            all.sort_unstable();
            all
        };
        assert_eq!(objects(&read_led), objects(&led));
        assert!(read(root, "another").is_none());
        // a byte changed anywhere, a file cut short
        let file = path(root, "one");
        let whole = std::fs::read(&file).unwrap();
        for at in [0, MAGIC.len() + 2, whole.len() / 2, whole.len() - 1] {
            let mut changed = whole.clone();
            changed[at] ^= 1;
            std::fs::write(&file, &changed).unwrap();
            assert!(read(root, "one").is_none(), "{at}");
        }
        std::fs::write(&file, &whole[..whole.len() - 3]).unwrap();
        assert!(read(root, "one").is_none());

        // a write that fails leaves nothing beside the file
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("in-the-way"), b"").unwrap();
        assert!(written(root, "one", &tips, &led).is_err());
        let names = |root: &Path| -> Vec<String> {
            let entries = std::fs::read_dir(directory(root)).unwrap();
            let mut names: Vec<String> = entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        let led_name = file.file_name().unwrap().to_str().unwrap().to_owned();
        assert_eq!(names(root), std::slice::from_ref(&led_name));
        // what a process that ended left beside the files is removed, and
        // nothing else is
        std::fs::remove_dir_all(&file).unwrap();
        written(root, "one", &tips, &led).unwrap();
        let left = [format!("{led_name}.4242"), format!("{led_name}.17")];
        for name in &left {
            std::fs::write(directory(root).join(name), b"half").unwrap();
        }
        std::fs::write(directory(root).join("other.led.x"), b"").unwrap();
        assert_eq!(left_behind_removed(root).unwrap(), 2);
        assert_eq!(names(root), [led_name, "other.led.x".to_owned()]);
        assert!(read(root, "one").is_some());
        assert_eq!(left_behind_removed(&root.join("none")).unwrap(), 0);
        assert_eq!(
            Led::again(vec![[2; 20], [2; 20]]).map(|led| led.len()),
            None
        );
    }
}
