//! Running a merge: begun or given up.

use std::io::Write;

use super::Res;
use crate::native::NativeRepo;
use crate::sequence_args::Merge;

/// git exits 1 when a merge stops in conflict
pub(super) fn merge<W: Write>(repo: &mut NativeRepo, args: &Merge, out: &mut W) -> Res<u8> {
    if args.abort_merge {
        repo.merge_abort(out)?;
        return Ok(0);
    }
    let branch = args.branch.as_deref().unwrap_or_default();
    Ok(u8::from(repo.merge(branch, args.json, out)?))
}
