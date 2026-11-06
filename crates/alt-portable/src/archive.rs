use crate::{Blob, BlobSource, Envelope, Error, PackageLimits, Result};
use alt_remote_protocol::Scope;
use std::io::{Read, Write};

const MAGIC: &[u8; 16] = b"ALT-PORTABLE-V1\0";
const END: &[u8; 16] = b"ALT-COMPLETE-V1\0";

fn copy_blob(reader: &mut dyn Read, writer: &mut dyn Write, blob: &Blob) -> Result<()> {
    let mut remaining = blob.bytes;
    let mut buffer = [0u8; 65536];
    let mut hash = blake3::Hasher::new();
    while remaining > 0 {
        let length = remaining.min(buffer.len() as u64) as usize;
        let count = reader.read(&mut buffer[..length])?;
        if count == 0 {
            return Err(Error::Invalid("truncated blob"));
        }
        hash.update(&buffer[..count]);
        writer.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    if hash.finalize().to_hex().as_str() != blob.blake3 {
        return Err(Error::Invalid("blob digest"));
    }
    Ok(())
}

/// No filenames, symlinks, compression or nested archives enter the wire
/// format. Payloads follow the authenticated, strictly ordered catalog.
pub fn write_archive(
    writer: &mut impl Write,
    envelope: &Envelope,
    source: &impl BlobSource,
    limits: PackageLimits,
) -> Result<()> {
    let key = alt_provenance::decode_hex(&envelope.catalog.source_public_key)?;
    let digest = envelope.verify(&envelope.catalog.source, &key, limits)?;
    let metadata = envelope.encode(limits)?;
    writer.write_all(MAGIC)?;
    writer.write_all(&(metadata.len() as u64).to_le_bytes())?;
    writer.write_all(&metadata)?;
    for blob in &envelope.catalog.blobs {
        let mut reader = source.open(blob)?;
        copy_blob(&mut reader, writer, blob)?;
        if reader.read(&mut [0])? != 0 {
            return Err(Error::Invalid("blob size"));
        }
    }
    writer.write_all(END)?;
    writer.write_all(&alt_provenance::decode_hex::<32>(&digest)?)?;
    writer.flush()?;
    Ok(())
}

/// Receivers must stage privately. Bytes passed to `stage` are untrusted until
/// this function returns successfully; errors may leave partial staging data.
/// No destination activation or semantic graph validation occurs here.
pub fn read_archive(
    reader: &mut impl Read,
    expected: &Scope,
    pinned_key: &[u8; 32],
    limits: PackageLimits,
    mut stage: impl FnMut(&Blob) -> Result<Box<dyn Write>>,
) -> Result<Envelope> {
    limits.validate()?;
    let mut magic = [0; 16];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(Error::Invalid("archive version"));
    }
    let mut length = [0; 8];
    reader.read_exact(&mut length)?;
    let length = u64::from_le_bytes(length);
    if length > limits.metadata_bytes as u64 {
        return Err(Error::Limit("archive metadata bytes"));
    }
    let mut raw = vec![0; length as usize];
    reader.read_exact(&mut raw)?;
    let envelope: Envelope = serde_json::from_slice(&raw)?;
    let digest = envelope.verify(expected, pinned_key, limits)?;
    // One canonical representation also rejects duplicate JSON map fields and
    // alternate encodings before any staging callback receives payload bytes.
    if envelope.encode(limits)? != raw {
        return Err(Error::Invalid("noncanonical envelope"));
    }
    for blob in &envelope.catalog.blobs {
        let mut writer = stage(blob)?;
        copy_blob(reader, &mut writer, blob)?;
        writer.flush()?;
    }
    reader.read_exact(&mut magic)?;
    let mut trailer = [0; 32];
    reader.read_exact(&mut trailer)?;
    if &magic != END || trailer != alt_provenance::decode_hex::<32>(&digest)? {
        return Err(Error::Invalid("archive completion marker"));
    }
    if reader.read(&mut [0])? != 0 {
        return Err(Error::Invalid("trailing archive bytes"));
    }
    Ok(envelope)
}
