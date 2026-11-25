//! The body of a receipt frame: how a request ended, and what it answered.
//!
//! ```text
//! [version u8, 1][request 16][binding 32][outcome u8][exit code u8]
//! [the frame of the request's first op u64, or zero][how many ops u32]
//! [when it finished, in milliseconds u64]
//! [blake3 of the response 32][its length u32][the response, when short]
//! ```
//!
//! A response of more than [`INLINE_MAX`] bytes is kept by whoever wrote
//! the receipt, under its digest.

const VERSION: u8 = 1;
const FIXED_LEN: usize = 1 + 16 + 32 + 1 + 1 + 8 + 4 + 8 + 32 + 4;
/// the longest response a receipt holds itself
pub const INLINE_MAX: usize = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Executed,
    Refused,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub request: [u8; 16],
    /// what the request was bound to: its arguments, who made it and where
    pub binding: [u8; 32],
    pub outcome: Outcome,
    pub exit: u8,
    pub first_op: u64,
    pub ops: u32,
    pub finished_ms: u64,
    pub digest: [u8; 32],
    pub len: u32,
    /// `None` for a response kept elsewhere
    pub response: Option<Vec<u8>>,
}

impl Receipt {
    /// Sets the response: its digest and length, and the bytes themselves
    /// when the receipt is to hold them.
    pub fn answer(&mut self, response: &[u8]) {
        self.digest = *alt_hash::blake3::hash(response).as_bytes();
        self.len = response.len() as u32;
        self.response = (response.len() <= INLINE_MAX).then(|| response.to_vec());
    }

    pub fn encode(&self) -> Vec<u8> {
        let held = self.response.as_deref().unwrap_or_default();
        let mut out = Vec::with_capacity(FIXED_LEN + held.len());
        out.push(VERSION);
        out.extend_from_slice(&self.request);
        out.extend_from_slice(&self.binding);
        out.push(match self.outcome {
            Outcome::Executed => 1,
            Outcome::Refused => 2,
            Outcome::Failed => 3,
        });
        out.push(self.exit);
        out.extend_from_slice(&self.first_op.to_le_bytes());
        out.extend_from_slice(&self.ops.to_le_bytes());
        out.extend_from_slice(&self.finished_ms.to_le_bytes());
        out.extend_from_slice(&self.digest);
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(held);
        out
    }

    /// `None` for a body of another version, cut short, or holding a
    /// response of another length or digest than it says.
    pub fn decode(body: &[u8]) -> Option<Self> {
        let (fixed, held) = body.split_at_checked(FIXED_LEN)?;
        if fixed[0] != VERSION {
            return None;
        }
        let outcome = match fixed[49] {
            1 => Outcome::Executed,
            2 => Outcome::Refused,
            3 => Outcome::Failed,
            _ => return None,
        };
        let len = u32::from_le_bytes(fixed[103..107].try_into().unwrap());
        let digest: [u8; 32] = fixed[71..103].try_into().unwrap();
        let response = match held.len() {
            0 if len as usize > INLINE_MAX => None,
            held_len if held_len == len as usize => {
                if *alt_hash::blake3::hash(held).as_bytes() != digest {
                    return None;
                }
                Some(held.to_vec())
            }
            _ => return None,
        };
        Some(Receipt {
            request: fixed[1..17].try_into().unwrap(),
            binding: fixed[17..49].try_into().unwrap(),
            outcome,
            exit: fixed[50],
            first_op: u64::from_le_bytes(fixed[51..59].try_into().unwrap()),
            ops: u32::from_le_bytes(fixed[59..63].try_into().unwrap()),
            finished_ms: u64::from_le_bytes(fixed[63..71].try_into().unwrap()),
            digest,
            len,
            response,
        })
    }
}
