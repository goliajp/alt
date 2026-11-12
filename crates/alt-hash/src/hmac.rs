//! HMAC-SHA-256 (RFC 2104) and PBKDF2-HMAC-SHA-256 (RFC 8018).

use crate::Sha256;

const BLOCK: usize = 64;

/// streaming HMAC-SHA-256 with the key schedule computed once
#[derive(Clone)]
pub struct HmacSha256 {
    inner: Sha256,
    outer: Sha256,
}

impl HmacSha256 {
    pub fn new(key: &[u8]) -> Self {
        let mut block = [0u8; BLOCK];
        if key.len() > BLOCK {
            block[..32].copy_from_slice(&crate::sha256(key));
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let mut inner = Sha256::new();
        inner.update(&block.map(|byte| byte ^ 0x36));
        let mut outer = Sha256::new();
        outer.update(&block.map(|byte| byte ^ 0x5c));
        Self { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finalize(mut self) -> [u8; 32] {
        self.outer.update(&self.inner.finalize());
        self.outer.finalize()
    }
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new(key);
    mac.update(data);
    mac.finalize()
}

/// PBKDF2 with HMAC-SHA-256 producing one 32-byte block
///
/// # Panics
///
/// Panics if `iterations` is zero.
pub fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    assert!(iterations > 0, "pbkdf2 needs at least one iteration");
    let keyed = HmacSha256::new(password);
    let mut first = keyed.clone();
    first.update(salt);
    first.update(&1u32.to_be_bytes());
    let mut block = first.finalize();
    let mut output = block;
    for _ in 1..iterations {
        let mut next = keyed.clone();
        next.update(&block);
        block = next.finalize();
        for (out, byte) in output.iter_mut().zip(block) {
            *out ^= byte;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // a key longer than one block is hashed first
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn hmac_streams_in_pieces() {
        let mut mac = HmacSha256::new(b"Jefe");
        mac.update(b"what do ya ");
        mac.update(b"want for nothing?");
        assert_eq!(
            mac.finalize(),
            hmac_sha256(b"Jefe", b"what do ya want for nothing?")
        );
    }

    #[test]
    fn pbkdf2_matches_rfc_7914() {
        assert_eq!(
            hex(&pbkdf2_hmac_sha256(b"passwd", b"salt", 1)),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc"
        );
        assert_eq!(
            hex(&pbkdf2_hmac_sha256(b"Password", b"NaCl", 80000)),
            "4ddcd8f60b98be21830cee5ef22701f9641a4418d04c0414aeff08876b34ab56"
        );
        assert_eq!(
            hex(&pbkdf2_hmac_sha256(b"password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }
}
