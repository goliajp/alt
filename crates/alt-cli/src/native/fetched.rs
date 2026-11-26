//! What a fetch brought: its pack taken into the odb, and the report of it.

use std::io::Write;

use alt_git_codec::HashAlgo;

use super::{NativeRepo, RefUpdate, Res};

impl NativeRepo<'_> {
    /// Takes the pack of the fetch response `response` into the odb; the
    /// bytes of the pack, none where the server had nothing new.
    pub(super) fn fetched_pack(
        &mut self,
        response: &mut dyn std::io::Read,
        algo: HashAlgo,
    ) -> Res<u64> {
        // drain preamble + sideband-wrapped pack stream into a temp .pack,
        // which is gone again unless it is read whole and indexed
        let pack_dir = self.store.alt_dir.join("pack");
        std::fs::create_dir_all(&pack_dir)?;
        stale_removed(&pack_dir)?;
        let mut reader = response;
        let preamble = alt_wire::read_fetch_preamble(&mut reader, algo)?;
        if preamble.packfile_missing {
            return Err("server returned no packfile (negotiation incomplete)".into());
        }
        // with the permissions the umask gives, as the pack it becomes
        let mut sink = alt_os::temp::NamedTempFile::new_plain_in(&pack_dir)?;
        let bytes_written = alt_wire::drain_packfile(&mut reader, sink.as_file_mut(), |_| {})?;
        // to its end: an answer cut short is an error of the transport
        std::io::copy(&mut reader, &mut std::io::sink())?;
        let (file, tmp_pack) = sink.into_parts();
        file.sync_all()?;
        drop(file);
        if bytes_written == 0 {
            // server said "nothing new": the empty file goes with `tmp_pack`
            return Ok(0);
        }
        // index the pack and ingest its objects into the native odb;
        // indexed, it has the name of its content
        let indexed = alt_git_pack::index_pack(&tmp_pack, algo, true)
            .map_err(|e| format!("index-pack failed: {e}"))?;
        tmp_pack.keep()?;
        let ip = alt_git_pack::IndexedPack::open(&indexed.pack_path, algo)?;
        let idx = ip.idx();
        let mut order: Vec<(u64, u32)> = (0..idx.len())
            .map(|i| (idx.offset_at(i).expect("idx in range"), i))
            .collect();
        order.sort_unstable();
        for (offset, i) in order {
            let obj = ip.read_at(offset)?;
            self.store.odb.put(idx.oid_at(i), obj.kind, &obj.data)?;
        }
        self.store.odb.flush()?;
        Ok(bytes_written)
    }
}

/// How long a temporary file of a fetch is let be before it is taken for
/// one that a fetch which was ended left behind, as Git's `gc.pruneExpire`
/// lets its temporary files be.
const STALE: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);

/// Removes from `pack_dir` the temporary files that fetches which were
/// ended before they could remove them left there: those not written to
/// for [`STALE`]. A fetch going on now writes to its own all along.
fn stale_removed(pack_dir: &std::path::Path) -> std::io::Result<()> {
    let now = std::time::SystemTime::now();
    for entry in std::fs::read_dir(pack_dir)? {
        let entry = entry?;
        if !entry.file_name().as_encoded_bytes().starts_with(b".tmp") {
            continue;
        }
        let meta = match entry.metadata() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            meta => meta?,
        };
        let age = now.duration_since(meta.modified()?).unwrap_or_default();
        if !meta.is_file() || age < STALE {
            continue;
        }
        // another fetch may have removed it meanwhile
        match std::fs::remove_file(entry.path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Writes what a fetch of `(objects, bytes_written)` did to the refs in `updates`.
pub(super) fn render(
    out: &mut impl Write,
    json: bool,
    remote_name: &str,
    (objects, bytes_written): (usize, u64),
    updates: &[RefUpdate],
) -> Res<()> {
    if json {
        use crate::json::Json;
        let refs_json: Vec<Json> = updates
            .iter()
            .map(|u| {
                Json::Object(vec![
                    ("server", Json::str(&u.server_name)),
                    ("local", Json::str(&u.local_name)),
                    ("oid", Json::str(u.new.to_string())),
                    (
                        "old",
                        match u.old {
                            Some(o) => Json::str(o.to_string()),
                            None => Json::Null,
                        },
                    ),
                ])
            })
            .collect();
        crate::json::emit(
            out,
            vec![
                ("remote", Json::str(remote_name)),
                ("objects", Json::Num(objects as i64)),
                ("pack_bytes", Json::Num(bytes_written as i64)),
                ("refs", Json::Array(refs_json)),
            ],
        )?;
    } else {
        writeln!(
            out,
            "fetch '{}': {} ref(s), {} byte(s) of packfile",
            remote_name,
            updates.len(),
            bytes_written
        )?;
        for u in updates {
            let status = match u.old {
                None => "new",
                Some(o) if o == u.new => "up-to-date",
                Some(_) => "updated",
            };
            writeln!(out, "  {status:>10}  {} -> {}", u.server_name, u.local_name)?;
        }
    }
    Ok(())
}
