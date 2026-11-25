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

fn path(root: &Path, repository: &str) -> PathBuf {
    let name = &alt_hash::blake3::hash(repository.as_bytes()).to_hex()[..32];
    root.join("packs")
        .join("led_computed")
        .join(format!("{name}.led"))
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
    let mut file = std::fs::File::create(&beside)?;
    file.write_all(&bytes)?;
    drop(file);
    std::fs::rename(&beside, &target)
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
        assert_eq!(
            Led::again(vec![[2; 20], [2; 20]]).map(|led| led.len()),
            None
        );
    }
}
