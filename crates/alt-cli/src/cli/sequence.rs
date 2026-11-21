//! Running a merge or a cherry-pick: begun, gone on with or given up.

use std::io::Write;

use super::Res;
use crate::native::NativeRepo;
use crate::sequence_args::{CherryPick, Merge};

/// git exits 1 when a merge stops in conflict
pub(super) fn merge<W: Write>(repo: &mut NativeRepo, args: &Merge, out: &mut W) -> Res<u8> {
    if args.abort_merge {
        repo.merge_abort(out)?;
        return Ok(0);
    }
    let branch = args.branch.as_deref().unwrap_or_default();
    Ok(u8::from(repo.merge(branch, args.json, out)?))
}

/// git exits 1 when a cherry-pick stops in conflict
pub(super) fn cherry_pick<W: Write>(
    repo: &mut NativeRepo,
    args: &CherryPick,
    out: &mut W,
) -> Res<u8> {
    if args.abort_pick {
        repo.cherry_pick_abort(out)?;
        return Ok(0);
    }
    if args.continue_pick {
        repo.cherry_pick_continue(args.json, out)?;
        return Ok(0);
    }
    let rev = args.rev.as_deref().unwrap_or_default();
    Ok(u8::from(repo.cherry_pick(rev, args.json, out)?))
}
