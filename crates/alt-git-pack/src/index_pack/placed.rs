//! Where a pack and the idx made for it are put: under the names of the
//! pack's trailer, without taking the place of a file that has one.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::PackError;

/// Writes `idx` next to the pack at `pack_path` and, with `rename`, gives
/// both the names of `trailer`; where they are then. A file that has one
/// of those names already is the same pack or idx, as Git takes it to be:
/// it stays, and the one of this call is removed. Where that fails, the
/// idx is not left behind, and the pack is where it was unless it was
/// removed for one that was there.
pub(super) fn placed(
    pack_path: &Path,
    idx: &[u8],
    trailer: &[u8],
    rename: bool,
    kept: bool,
) -> Result<(PathBuf, PathBuf), PackError> {
    let idx_path = pack_path.with_extension("idx");
    let tmp_idx = pack_path.with_extension("idx.tmp");
    let written =
        write_idx(&tmp_idx, idx, kept).and_then(|()| std::fs::rename(&tmp_idx, &idx_path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp_idx);
        return Err(e.into());
    }
    if !rename {
        return Ok((pack_path.to_path_buf(), idx_path));
    }
    let dir = pack_path.parent().unwrap_or_else(|| Path::new("."));
    let hex: String = trailer.iter().map(|b| format!("{b:02x}")).collect();
    let renamed_pack = dir.join(format!("pack-{hex}.pack"));
    let renamed_idx = dir.join(format!("pack-{hex}.idx"));
    // the idx last: a pack is taken up once its idx is there
    let moved = settled(pack_path, &renamed_pack).and_then(|ours| {
        let Err(e) = settled(&idx_path, &renamed_idx) else {
            return Ok(());
        };
        // the pack goes back to whoever has it under its own name
        if ours {
            std::fs::rename(&renamed_pack, pack_path)?;
        }
        Err(e)
    });
    if let Err(e) = moved {
        if idx_path != renamed_idx {
            let _ = std::fs::remove_file(&idx_path);
        }
        return Err(e.into());
    }
    Ok((renamed_pack, renamed_idx))
}

/// Gives the file at `from` the name `to` where no file has it; where one
/// has, that one stays and `from` is removed. Whether `from` took the name.
fn settled(from: &Path, to: &Path) -> std::io::Result<bool> {
    if from == to {
        return Ok(false);
    }
    match alt_os::temp::rename_exclusive(from, to) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // what has the name and is no file is in the way
            if !std::fs::symlink_metadata(to)?.is_file() {
                return Err(e);
            }
            std::fs::remove_file(from)?;
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

fn write_idx(path: &Path, idx: &[u8], kept: bool) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(idx)?;
    if kept {
        f.sync_all()?;
    }
    Ok(())
}
