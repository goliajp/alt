//! Argon2id (RFC 9106, version 0x13) and its PHC string encoding.

pub mod block;
mod fill;
mod phc;

use alt_hash::Blake2b;
use block::Block;
use std::fmt;

pub use phc::PasswordHash;

/// The work factors of one Argon2id computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// memory in KiB
    pub m_cost: u32,
    /// passes over the memory
    pub t_cost: u32,
    /// lanes; they are filled one after another, so this changes the result but not the speed
    pub p_cost: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A work factor or input length outside what RFC 9106 allows.
    Params,
    /// A salt shorter than 8 bytes.
    Salt,
    /// A tag shorter than 4 bytes.
    OutputLength,
    /// Not a PHC string this implementation produces.
    Encoding,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Params => "argon2 work factors out of range",
            Self::Salt => "argon2 salt shorter than 8 bytes",
            Self::OutputLength => "argon2 tag shorter than 4 bytes",
            Self::Encoding => "not an argon2id password hash string",
        })
    }
}

impl std::error::Error for Error {}

const ARGON2ID: u32 = 2;
const VERSION: u32 = 0x13;
const SLICES: usize = 4;

/// The Argon2id tag of `password` under `salt`, `N` bytes long.
pub fn hash_password<const N: usize>(
    password: &[u8],
    salt: &[u8],
    params: &Params,
) -> Result<[u8; N], Error> {
    let mut tag = [0u8; N];
    hash_password_into(password, salt, params, &mut tag)?;
    Ok(tag)
}

/// Fills `tag` with the Argon2id tag of `password` under `salt`.
pub fn hash_password_into(
    password: &[u8],
    salt: &[u8],
    params: &Params,
    tag: &mut [u8],
) -> Result<(), Error> {
    hash_keyed_into(password, salt, &[], &[], params, tag)
}

/// [`hash_password_into`] with the optional secret key and associated data
/// of RFC 9106, both of which change the tag.
pub fn hash_keyed_into(
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    associated: &[u8],
    params: &Params,
    tag: &mut [u8],
) -> Result<(), Error> {
    let Params {
        m_cost,
        t_cost,
        p_cost,
    } = *params;
    if !(1..1 << 24).contains(&p_cost) || t_cost == 0 || u64::from(m_cost) < 8 * u64::from(p_cost) {
        return Err(Error::Params);
    }
    if salt.len() < 8 || u32::try_from(salt.len()).is_err() {
        return Err(Error::Salt);
    }
    if tag.len() < 4 || u32::try_from(tag.len()).is_err() {
        return Err(Error::OutputLength);
    }
    if [password, secret, associated]
        .iter()
        .any(|field| u32::try_from(field.len()).is_err())
    {
        return Err(Error::Params);
    }

    let lanes = p_cost as usize;
    let segment = m_cost as usize / (lanes * SLICES);
    let lane_len = segment * SLICES;
    let layout = fill::Layout {
        lanes,
        lane_len,
        segment,
        passes: t_cost,
    };

    let mut seed = Blake2b::new(64);
    for word in [p_cost, tag.len() as u32, m_cost, t_cost, VERSION, ARGON2ID] {
        seed.update(&word.to_le_bytes());
    }
    for field in [password, salt, secret, associated] {
        seed.update(&(field.len() as u32).to_le_bytes());
        seed.update(field);
    }
    let mut h0 = [0u8; 72];
    seed.finalize(&mut h0[..64]);

    let mut memory = vec![[0u64; 128]; lanes * lane_len];
    for lane in 0..lanes {
        h0[68..].copy_from_slice(&(lane as u32).to_le_bytes());
        for index in 0..2u32 {
            h0[64..68].copy_from_slice(&index.to_le_bytes());
            let mut bytes = [0u8; 1024];
            long_hash(&h0, &mut bytes);
            load(&mut memory[lane * lane_len + index as usize], &bytes);
        }
    }

    for pass in 0..t_cost {
        for slice in 0..SLICES {
            for lane in 0..lanes {
                fill::segment(&mut memory, &layout, pass, slice, lane);
            }
        }
    }

    let mut last = memory[lane_len - 1];
    for lane in 1..lanes {
        for (word, other) in last.iter_mut().zip(&memory[(lane + 1) * lane_len - 1]) {
            *word ^= other;
        }
    }
    let mut bytes = [0u8; 1024];
    for (chunk, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(last) {
        *chunk = word.to_le_bytes();
    }
    long_hash(&bytes, tag);
    Ok(())
}

fn load(block: &mut Block, bytes: &[u8; 1024]) {
    for (word, chunk) in block.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
}

/// the variable-length hash H' of RFC 9106 section 3.3
fn long_hash(input: &[u8], out: &mut [u8]) {
    let length = (out.len() as u32).to_le_bytes();
    if out.len() <= 64 {
        let mut hasher = Blake2b::new(out.len());
        hasher.update(&length);
        hasher.update(input);
        hasher.finalize(out);
        return;
    }
    let mut hasher = Blake2b::new(64);
    hasher.update(&length);
    hasher.update(input);
    let mut link = [0u8; 64];
    hasher.finalize(&mut link);
    // every link contributes its first half, the last one all of itself
    let mut rest = out;
    while rest.len() > 64 {
        let (head, tail) = rest.split_at_mut(32);
        head.copy_from_slice(&link[..32]);
        rest = tail;
        let mut hasher = Blake2b::new(if rest.len() > 64 { 64 } else { rest.len() });
        hasher.update(&link);
        hasher.finalize(&mut link[..rest.len().min(64)]);
    }
    rest.copy_from_slice(&link[..rest.len()]);
}
