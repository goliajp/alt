//! The plain entries of a pack the scan kept: hashed and taken in, a batch a thread.

use crate::{
    Error, Kind,
    descend::Made,
    ingest::Meta,
    scan::{Form, Raw},
    told::Told,
};

/// The object ids of a batch are computed together, several messages per step of
/// the hash; batches run in parallel.
pub(crate) fn hashed(entries: &[Raw], told: &Told<'_>) -> Result<Made, Error> {
    let kept: Vec<(usize, Kind, &[u8])> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, raw)| match (raw.form, raw.data.as_deref()) {
            (Form::Plain(kind), Some(data)) => Some((index, kind, data)),
            _ => None,
        })
        .collect();
    let batches: Vec<&[(usize, Kind, &[u8])]> = kept.chunks(32).collect();
    let parts = alt_threads::map(&batches, 1, |batch| -> Result<Made, Error> {
        let headers: Vec<String> = batch
            .iter()
            .map(|(_, kind, data)| crate::object_header(*kind, data.len()))
            .collect();
        let parts: Vec<[&[u8]; 2]> = headers
            .iter()
            .zip(*batch)
            .map(|(header, (_, _, data))| [header.as_bytes(), *data])
            .collect();
        let messages: Vec<&[&[u8]]> = parts.iter().map(|pair| pair.as_slice()).collect();
        let oids = alt_hash::sha1_many(&messages);
        let contents: Vec<&[u8]> = batch.iter().map(|(_, _, data)| *data).collect();
        let digests = alt_hash::blake3::hash_many(&contents);
        let mut made = Made::default();
        for (((index, kind, data), oid), digest) in batch.iter().zip(oids).zip(digests) {
            (made.commits).extend(told.object(*index as u32, &oid, *kind, data)?);
            let meta = Meta::plain(oid, *digest.as_bytes(), *kind, data.len() as u64);
            made.metas.push((*index, meta));
        }
        Ok(made)
    });
    let mut all = Made::default();
    for part in parts {
        let part = part?;
        all.metas.extend(part.metas);
        all.commits.extend(part.commits);
    }
    Ok(all)
}
