//! Git smart HTTP reads over verified pack segments: the advertisement, the reference
//! listing and the pack a fetch asks for, all from stored bytes.

use crate::{
    Output,
    git::Command,
    git_packs::{id, raw, segment_error},
    git_response::Spill,
};
use alt_git_codec::{HashAlgo, ObjectKind};
use alt_hash::KeyedIdSet;
use alt_pack_segment::{Kind, Oid, SegmentSet};
use alt_repository_service::{Node, parse_node};
use alt_server_store::{Error, PackView, RefScope, Result};
use alt_wire::pkt;

const UPLOAD_RESULT: &str = "application/x-git-upload-pack-result";

fn peel(set: &SegmentSet, mut oid: Oid, remaining: &mut usize) -> Result<Option<Oid>> {
    let mut peeled = None;
    while set.kind(&oid).map_err(segment_error)? == Some(Kind::Tag) {
        *remaining = remaining
            .checked_sub(1)
            .ok_or(Error::Limit("tag peel walk"))?;
        let (_, data) = set.read(&oid).map_err(segment_error)?;
        let Node::Tag { target, .. } = parse_node(ObjectKind::Tag, &data, HashAlgo::Sha1)? else {
            return Err(Error::Integrity("tag object"));
        };
        oid = raw(target);
        peeled = Some(oid);
    }
    Ok(peeled)
}

/// The commits among `said`, what the client says it has, that the tips of the
/// refs the caller reads lead to. One that is in the segments and that none of
/// them leads to, of a branch that was deleted or history that was replaced, is
/// not acknowledged, and nothing is left out of a pack on the strength of it.
pub(crate) fn held(
    set: &SegmentSet,
    tips: &KeyedIdSet<Oid>,
    said: &[alt_git_codec::ObjectId],
    remaining: &mut usize,
    steps: usize,
) -> Result<Vec<Oid>> {
    let said: Vec<Oid> = said.iter().map(|oid| raw(*oid)).collect();
    if said.is_empty() {
        return Ok(said);
    }
    let mut heads = Vec::with_capacity(tips.len());
    for tip in tips {
        heads.push(peel(set, *tip, remaining)?.unwrap_or(*tip));
    }
    set.reached(&heads, &said, steps).map_err(segment_error)
}

/// What lists references, written as the pack of a fetch is: the references are
/// as many as were pushed, and a long list of them is a file of the room for
/// responses, not memory of the request.
fn listed(
    sending: Option<alt_shares::Share>,
    content_type: &'static str,
    write: impl FnOnce(&mut Spill) -> Result<()>,
) -> Result<Output> {
    let mut body = Spill::within(sending);
    let written = write(&mut body);
    body.into_output(content_type, written)
}

fn advertisement(
    receive: bool,
    view: &PackView,
    sending: Option<alt_shares::Share>,
) -> Result<Output> {
    if receive {
        let refs: Vec<_> = view
            .refs
            .iter()
            .map(|(name, oid)| (name.to_owned(), oid))
            .collect();
        let capabilities = [
            "report-status",
            "atomic",
            "delete-refs",
            "ofs-delta",
            "side-band-64k",
            "object-format=sha1",
            "agent=alt-server",
        ];
        let content_type = "application/x-git-receive-pack-advertisement";
        return listed(sending, content_type, |body| {
            let algo = HashAlgo::Sha1;
            Ok(alt_wire::push::encode_v1_ref_advertisement(
                body,
                &refs,
                &capabilities,
                algo,
            )?)
        });
    }
    let mut output = Vec::new();
    pkt::write_data(&mut output, b"# service=git-upload-pack\n")?;
    pkt::write_flush(&mut output)?;
    for line in [
        "version 2\n",
        "agent=alt-server\n",
        "ls-refs=unborn\n",
        "fetch=wait-for-done\n",
        "object-format=sha1\n",
    ] {
        pkt::write_data(&mut output, line.as_bytes())?;
    }
    pkt::write_flush(&mut output)?;
    Ok(Output::Bytes {
        content_type: "application/x-git-upload-pack-advertisement",
        bytes: output,
    })
}

fn references(
    request: &alt_wire::ls_refs::LsRefsRequest,
    unborn: bool,
    view: &PackView,
    sending: Option<alt_shares::Share>,
    mut remaining: usize,
) -> Result<Output> {
    let prefixes = request.prefixes();
    let wanted = |name: &str| alt_wire::ls_refs::LsRefsRequest::listed(&prefixes, name);
    let head = view.refs.get(&view.default_branch);
    listed(sending, UPLOAD_RESULT, |body| {
        // HEAD sorts before every name under refs/
        for (name, oid) in head
            .map(|oid| ("HEAD", oid))
            .into_iter()
            .chain(view.refs.iter())
        {
            if !wanted(name) {
                continue;
            }
            let mut line = format!("{oid} {name}");
            if request.symrefs && name == "HEAD" {
                line.push_str(&format!(" symref-target:{}", view.default_branch));
            }
            if request.peel
                && let Some(target) = peel(&view.segments, raw(oid), &mut remaining)?
            {
                line.push_str(&format!(" peeled:{}", id(&target)));
            }
            line.push('\n');
            pkt::write_data(body, line.as_bytes())?;
        }
        if head.is_none() && unborn && wanted("HEAD") {
            let line = format!("unborn HEAD symref-target:{}\n", view.default_branch);
            pkt::write_data(body, line.as_bytes())?;
        }
        Ok(pkt::write_flush(body)?)
    })
}

pub(crate) fn read(
    limits: alt_remote_protocol::Limits,
    receive: bool,
    command: &Command,
    view: PackView,
    sending: Option<alt_shares::Share>,
    gone: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Output> {
    let bytes = |content_type, bytes| {
        Ok(Output::Bytes {
            content_type,
            bytes,
        })
    };
    let mut output = Vec::new();
    let mut remaining = limits.walk_steps;
    match command {
        Command::Advertisement => return advertisement(receive, &view, sending),
        Command::Refs(request, unborn) => {
            return references(request, *unborn, &view, sending, remaining);
        }
        Command::Fetch(_) => {}
    }
    let Command::Fetch(request) = command else {
        return Err(Error::Integrity("Git command dispatch"));
    };
    if request.wants.is_empty() {
        return Err(Error::Integrity("empty Git wants"));
    }
    // as in Git's default configuration, only current ref tips can be asked for
    let tips: KeyedIdSet<Oid> = view.refs.values().map(raw).collect();
    let wants: Vec<Oid> = request.wants.iter().map(|oid| raw(*oid)).collect();
    if wants.iter().any(|oid| !tips.contains(oid)) {
        return Err(Error::Unauthorized);
    }
    let set = &view.segments;
    let haves = held(
        set,
        &tips,
        &request.haves,
        &mut remaining,
        limits.walk_steps,
    )?;
    if !request.done {
        pkt::write_data(&mut output, b"acknowledgments\n")?;
        if haves.is_empty() {
            pkt::write_data(&mut output, b"NAK\n")?;
        } else {
            for oid in &haves {
                pkt::write_data(&mut output, format!("ACK {}\n", id(oid)).as_bytes())?;
            }
        }
        pkt::write_flush(&mut output)?;
        return bytes(UPLOAD_RESULT, output);
    }
    let every_tip = match view.ref_scope {
        RefScope::Filtered { other_tips } => !other_tips,
        // every want is a tip, and the request has each of its wants once
        _ => wants.len() == tips.len(),
    };
    let everything = haves.is_empty() && !view.unreachable && set.verbatim() && every_tip;
    if everything {
        // nothing is computed per object: the stored bytes go out as they are read
        let (pieces, checksum) = set.whole().map_err(segment_error)?;
        return Ok(Output::WholePack(pieces, checksum));
    }
    // the trees a fetch may read are as many as any walk may take steps; one
    // nobody waits for any more ends at the next
    let select = |wants: &[Oid]| set.select_while(wants, &haves, limits.walk_steps, gone);
    let mut selection = select(&wants).map_err(segment_error)?;
    if request.include_tag {
        let chosen: KeyedIdSet<&Oid> = selection.objects().iter().collect();
        let mut more = wants.clone();
        for (name, oid) in view.refs.iter() {
            let tag = raw(oid);
            if name.starts_with("refs/tags/")
                && !chosen.contains(&tag)
                && let Some(target) = peel(set, tag, &mut remaining)?
                && chosen.contains(&target)
            {
                more.push(tag);
            }
        }
        if more.len() > wants.len() {
            selection = select(&more).map_err(segment_error)?;
        }
    }
    // the pack is framed as it is written; a large one is not kept in memory
    let mut body = Spill::within(sending);
    let written = (|| {
        pkt::write_data(&mut body, b"packfile\n")?;
        let mut band = crate::git_response::Band::new(&mut body);
        set.write_selected(&selection, request.thin_pack, &mut band)
            .map_err(segment_error)?;
        band.finish()?;
        Ok(pkt::write_flush(&mut body)?)
    })();
    body.into_output(UPLOAD_RESULT, written)
}
