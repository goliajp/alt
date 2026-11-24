//! Running a merge, a cherry-pick, a revert or a rebase: begun, gone on
//! with or given up.

use std::io::Write;

use super::Res;
use crate::native::NativeRepo;
use crate::sequence_args::{CherryPick, Merge, Revert};

/// git exits 1 when a merge stops in conflict
pub(super) fn merge<W: Write>(repo: &mut NativeRepo, args: &Merge, out: &mut W) -> Res<u8> {
    if args.abort_merge {
        repo.merge_abort(out)?;
        return Ok(0);
    }
    if args.continue_merge {
        repo.merge_continue(args.json, out)?;
        return Ok(0);
    }
    if args.quit_merge {
        repo.merge_quit(out)?;
        return Ok(0);
    }
    repo.ensure_no_pick_to_conclude()?;
    let branch = args.branch.as_deref().unwrap_or_default();
    Ok(u8::from(repo.merge(
        branch,
        args.fast_forward(),
        args.json,
        out,
    )?))
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
    if args.skip_pick {
        repo.cherry_pick_skip(out)?;
        return Ok(0);
    }
    if args.quit_pick {
        repo.cherry_pick_quit(out)?;
        return Ok(0);
    }
    let rev = args.rev.as_deref().unwrap_or_default();
    Ok(u8::from(repo.cherry_pick(rev, args.noted, args.json, out)?))
}

/// git exits 1 when a rebase stops
pub(super) fn rebase<W: Write>(
    repo: &mut NativeRepo,
    args: &crate::commit_args::Rebase,
    out: &mut W,
) -> Res<u8> {
    if args.abort {
        repo.rebase_abort(out)?;
        return Ok(0);
    }
    if args.quit_rebase {
        repo.rebase_quit(out)?;
        return Ok(0);
    }
    let stopped = if args.cont {
        repo.rebase_continue(args.json, out)?
    } else if args.skip {
        repo.rebase_skip(args.json, out)?
    } else {
        repo.rebase(args, out)?
    };
    Ok(u8::from(stopped))
}

/// git exits 1 when a revert stops in conflict
pub(super) fn revert<W: Write>(repo: &mut NativeRepo, args: &Revert, out: &mut W) -> Res<u8> {
    if args.abort {
        repo.revert_abort(out)?;
        return Ok(0);
    }
    if args.quit_revert {
        repo.revert_quit(out)?;
        return Ok(0);
    }
    let stopped = if args.cont {
        repo.revert_continue(args.json, out)?
    } else if args.skip_revert {
        repo.revert_skip(args.json, out)?
    } else {
        repo.revert(&args.revs, args.mainline, args.json, out)?
    };
    Ok(u8::from(stopped))
}
