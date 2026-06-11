use std::path::Path;

use alt_git_codec::HashAlgo;
use alt_git_pack::IndexedPack;

use crate::RepoError;

/// Opens every `*.pack` under `objects/pack`.
pub(crate) fn open_packs(pack_dir: &Path, algo: HashAlgo) -> Result<Vec<IndexedPack>, RepoError> {
    let entries = match std::fs::read_dir(pack_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut packs = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "pack") {
            packs.push(IndexedPack::open(&path, algo)?);
        }
    }
    Ok(packs)
}
