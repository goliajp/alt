//! `$argon2id$v=19$m=..,t=..,p=..$<salt>$<hash>` strings.

use super::{Error, Params, hash_password_into};
use crate::ct;
use alt_base64::STANDARD_NO_PAD;
use std::fmt;

/// An Argon2id tag with the salt and work factors that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasswordHash {
    pub params: Params,
    pub salt: Vec<u8>,
    pub hash: Vec<u8>,
}

impl PasswordHash {
    /// Hashes `password` under `salt` into a tag of `hash_len` bytes.
    pub fn new(
        password: &[u8],
        salt: &[u8],
        params: &Params,
        hash_len: usize,
    ) -> Result<Self, Error> {
        let mut hash = vec![0; hash_len];
        hash_password_into(password, salt, params, &mut hash)?;
        Ok(Self {
            params: *params,
            salt: salt.to_vec(),
            hash,
        })
    }

    /// Parses a PHC string.
    ///
    /// Only Argon2id version 19 with exactly the `m`, `t` and `p` parameters
    /// is accepted. Nothing is computed, so a caller can bound the work
    /// factors before calling [`PasswordHash::verify`].
    pub fn parse(encoded: &str) -> Result<Self, Error> {
        let mut fields = encoded.split('$');
        let mut next = || fields.next().ok_or(Error::Encoding);
        if !next()?.is_empty() || next()? != "argon2id" || next()? != "v=19" {
            return Err(Error::Encoding);
        }
        let mut costs = [None; 3];
        for pair in next()?.split(',') {
            let (name, value) = pair.split_once('=').ok_or(Error::Encoding)?;
            let slot = match name {
                "m" => &mut costs[0],
                "t" => &mut costs[1],
                "p" => &mut costs[2],
                _ => return Err(Error::Encoding),
            };
            if slot.replace(decimal(value)?).is_some() {
                return Err(Error::Encoding);
            }
        }
        let [Some(m_cost), Some(t_cost), Some(p_cost)] = costs else {
            return Err(Error::Encoding);
        };
        let salt = STANDARD_NO_PAD
            .decode(next()?)
            .map_err(|_| Error::Encoding)?;
        let hash = STANDARD_NO_PAD
            .decode(next()?)
            .map_err(|_| Error::Encoding)?;
        if fields.next().is_some() || salt.is_empty() || hash.is_empty() {
            return Err(Error::Encoding);
        }
        Ok(Self {
            params: Params {
                m_cost,
                t_cost,
                p_cost,
            },
            salt,
            hash,
        })
    }

    /// Whether `password` hashes to the stored tag, compared in constant time.
    pub fn verify(&self, password: &[u8]) -> Result<bool, Error> {
        let mut computed = vec![0; self.hash.len()];
        hash_password_into(password, &self.salt, &self.params, &mut computed)?;
        Ok(ct::eq(&computed, &self.hash))
    }
}

impl fmt::Display for PasswordHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Params {
            m_cost,
            t_cost,
            p_cost,
        } = self.params;
        write!(
            f,
            "$argon2id$v=19$m={m_cost},t={t_cost},p={p_cost}${}${}",
            STANDARD_NO_PAD.encode(&self.salt),
            STANDARD_NO_PAD.encode(&self.hash)
        )
    }
}

/// a decimal number as the encoder writes it: digits only, no leading zero
fn decimal(text: &str) -> Result<u32, Error> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if !canonical {
        return Err(Error::Encoding);
    }
    text.parse().map_err(|_| Error::Encoding)
}
