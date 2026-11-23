//! The `config` of the git repository an export makes.

use std::fs;
use std::path::Path;

use alt_git_codec::HashAlgo;

use crate::ExportError;

/// Writes the config, which declares the object format: what the store
/// kept of the git repository it was imported from with the store's own
/// settings written in, or the minimum a repository needs. With it go
/// the other configuration files the import kept.
pub(crate) fn write_config(alt_dir: &Path, git_dir: &Path) -> Result<HashAlgo, ExportError> {
    // the export writes files-backend refs by decision, so a preserved
    // extensions.refstorage (e.g. reftable) must not survive; everything
    // else of a kept copy returns byte-identical
    let Some(bytes) = alt_repo::kept_config::for_git(alt_dir, strip_refstorage)? else {
        // native-born store without an import config
        fs::write(
            git_dir.join("config"),
            b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        )?;
        return Ok(HashAlgo::Sha1);
    };
    let algo = alt_repo::native_object_format(alt_dir)?;
    fs::write(git_dir.join("config"), bytes)?;
    alt_repo::kept_config::give(alt_dir, git_dir)?;
    Ok(algo)
}

/// Drops `refstorage = …` lines inside `[extensions]`, byte-preserving
/// everything else.
fn strip_refstorage(config: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(config.len());
    let mut in_extensions = false;
    for line in config.split_inclusive(|&b| b == b'\n') {
        let trimmed = line.trim_ascii();
        if trimmed.starts_with(b"[") {
            in_extensions = trimmed.eq_ignore_ascii_case(b"[extensions]");
        }
        if in_extensions {
            let lower = trimmed.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix(b"refstorage")
                && rest.trim_ascii_start().starts_with(b"=")
            {
                continue;
            }
        }
        out.extend_from_slice(line);
    }
    out
}
