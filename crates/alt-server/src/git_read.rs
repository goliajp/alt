//! Git smart HTTP reads over verified pack segments: the advertisement, the reference
//! listing and the pack a fetch asks for, all from stored bytes.

use crate::{
    Output,
    git::Command,
    git_packs::{id, raw, segment_error},
};
use alt_git_codec::{HashAlgo, ObjectKind};
use alt_pack_segment::{Kind, Oid, SegmentSet};
use alt_repository_service::{Node, parse_node};
use alt_server_store::{Error, PackView, RefScope, Result};
use alt_wire::pkt;
use std::collections::HashSet;

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

pub(crate) fn read(
    limits: alt_remote_protocol::Limits,
    receive: bool,
    command: &Command,
    view: PackView,
) -> Result<Output> {
    let bytes = |content_type, bytes| {
        Ok(Output::Bytes {
            content_type,
            bytes,
        })
    };
    let mut output = Vec::new();
    if matches!(command, Command::Advertisement) {
        if receive {
            let refs: Vec<_> = view
                .refs
                .iter()
                .map(|(name, oid)| (name.to_owned(), oid))
                .collect();
            alt_wire::push::encode_v1_ref_advertisement(
                &mut output,
                &refs,
                &[
                    "report-status",
                    "atomic",
                    "delete-refs",
                    "ofs-delta",
                    "side-band-64k",
                    "object-format=sha1",
                    "agent=alt-server",
                ],
                HashAlgo::Sha1,
            )?;
            return bytes("application/x-git-receive-pack-advertisement", output);
        }
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
        return bytes("application/x-git-upload-pack-advertisement", output);
    }
    let mut remaining = limits.walk_steps;
    if let Command::Refs(request, unborn) = command {
        let wanted = |name: &str| {
            request.ref_prefixes.is_empty()
                || request
                    .ref_prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        };
        let head = view.refs.get(&view.default_branch);
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
            pkt::write_data(&mut output, line.as_bytes())?;
        }
        if head.is_none() && *unborn && wanted("HEAD") {
            pkt::write_data(
                &mut output,
                format!("unborn HEAD symref-target:{}\n", view.default_branch).as_bytes(),
            )?;
        }
        pkt::write_flush(&mut output)?;
        return bytes(UPLOAD_RESULT, output);
    }
    let Command::Fetch(request) = command else {
        return Err(Error::Integrity("Git command dispatch"));
    };
    if request.wants.is_empty() {
        return Err(Error::Integrity("empty Git wants"));
    }
    // as in Git's default configuration, only current ref tips can be asked for
    let tips: HashSet<Oid> = view.refs.values().map(raw).collect();
    let wants: Vec<Oid> = request.wants.iter().map(|oid| raw(*oid)).collect();
    if wants.iter().any(|oid| !tips.contains(oid)) {
        return Err(Error::Unauthorized);
    }
    let set = &view.segments;
    let haves: Vec<Oid> = request
        .haves
        .iter()
        .map(|oid| raw(*oid))
        .filter(|oid| set.commit(oid).is_some())
        .collect();
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
        _ => tips.iter().all(|tip| wants.contains(tip)),
    };
    let everything = haves.is_empty() && !view.unreachable && set.verbatim() && every_tip;
    if everything {
        // nothing is computed per object: the stored bytes go out as they are read
        let (pieces, checksum) = set.whole().map_err(segment_error)?;
        return Ok(Output::WholePack(pieces, checksum));
    }
    let mut selection = set.select(&wants, &haves).map_err(segment_error)?;
    if request.include_tag {
        let chosen: HashSet<&Oid> = selection.objects().iter().collect();
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
            selection = set.select(&more, &haves).map_err(segment_error)?;
        }
    }
    // the pack is framed as it is written; a large one is not kept in memory
    let mut body = crate::git_response::Spill::new();
    pkt::write_data(&mut body, b"packfile\n")?;
    let mut band = crate::git_response::Band::new(&mut body);
    set.write_selected(&selection, request.thin_pack, &mut band)
        .map_err(segment_error)?;
    band.finish()?;
    pkt::write_flush(&mut body)?;
    body.into_output(UPLOAD_RESULT)
}
