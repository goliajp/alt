//! What a fetch brought: its pack taken into the odb, and the report of it.

use std::io::Write;

use alt_git_codec::HashAlgo;

use super::{NativeRepo, RefUpdate, Res};

impl NativeRepo<'_> {
    /// Takes the pack of the fetch response `response` into the odb; the
    /// bytes of the pack, none where the server had nothing new.
    pub(super) fn fetched_pack(
        &mut self,
        remote_name: &str,
        response: &[u8],
        algo: HashAlgo,
    ) -> Res<u64> {
        // drain preamble + sideband-wrapped pack stream into a temp .pack
        let pack_dir = self.store.alt_dir.join("pack");
        std::fs::create_dir_all(&pack_dir)?;
        let tmp_pack = pack_dir.join(format!("fetch-{}-incoming.pack", remote_name));
        let mut reader = response;
        let preamble = alt_wire::read_fetch_preamble(&mut reader, algo)?;
        if preamble.packfile_missing {
            return Err("server returned no packfile (negotiation incomplete)".into());
        }
        let bytes_written = {
            let mut sink = std::fs::File::create(&tmp_pack)?;
            let n = alt_wire::drain_packfile(&mut reader, &mut sink, |_| {})?;
            sink.sync_all()?;
            n
        };
        if bytes_written == 0 {
            // server said "nothing new" — drop the empty file, skip indexing
            let _ = crate::durable::remove_file(&tmp_pack);
        } else {
            // index the pack and ingest its objects into the native odb
            let indexed = alt_git_pack::index_pack(&tmp_pack, algo, true)
                .map_err(|e| format!("index-pack failed: {e}"))?;
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
        }
        Ok(bytes_written)
    }
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
