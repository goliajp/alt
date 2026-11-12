//! SCRAM-SHA-256 (RFC 5802, RFC 7677) without channel binding.

use crate::{Error, Result};
use alt_base64::STANDARD;
use alt_hash::{hmac_sha256, pbkdf2_hmac_sha256, sha256};

pub(crate) const MECHANISM: &str = "SCRAM-SHA-256";
const REFUSED: Error = Error::Authentication("malformed SCRAM message from the server");
/// far above any server default; bounds the work a server can demand
const MAX_ITERATIONS: u32 = 10_000_000;

pub(crate) struct Exchange {
    password: Vec<u8>,
    first_bare: String,
    nonce: String,
    server_signature: [u8; 32],
}

impl Exchange {
    /// `nonce` must be printable ASCII without commas
    pub fn new(user: &str, password: &str, nonce: String) -> Result<Self> {
        // SASLprep leaves ASCII unchanged; other passwords would need its
        // Unicode mapping tables
        if !password.is_ascii() {
            return Err(Error::Authentication(
                "passwords outside ASCII are not supported",
            ));
        }
        Ok(Self {
            password: password.into(),
            first_bare: format!("n={user},r={nonce}"),
            nonce,
            server_signature: [0; 32],
        })
    }

    pub fn client_first(&self) -> String {
        format!("n,,{}", self.first_bare)
    }

    /// consumes the server's first message and produces the client's final one
    pub fn client_final(&mut self, server_first: &[u8]) -> Result<String> {
        let server_first = std::str::from_utf8(server_first).map_err(|_| REFUSED)?;
        let mut fields = server_first.split(',');
        let nonce = field(fields.next(), "r=")?;
        let salt = field(fields.next(), "s=")?;
        let iterations = field(fields.next(), "i=")?;
        if !nonce.starts_with(&self.nonce) || nonce.len() == self.nonce.len() {
            return Err(Error::Authentication(
                "server did not extend the client nonce",
            ));
        }
        let salt = STANDARD.decode(salt).map_err(|_| REFUSED)?;
        let iterations: u32 = iterations.parse().map_err(|_| REFUSED)?;
        if iterations == 0 || iterations > MAX_ITERATIONS {
            return Err(REFUSED);
        }
        let salted = pbkdf2_hmac_sha256(&self.password, &salt, iterations);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        let without_proof = format!("c=biws,r={nonce}");
        let message = format!("{},{server_first},{without_proof}", self.first_bare);
        let signature = hmac_sha256(&stored_key, message.as_bytes());
        let mut proof = client_key;
        for (byte, mask) in proof.iter_mut().zip(signature) {
            *byte ^= mask;
        }
        let server_key = hmac_sha256(&salted, b"Server Key");
        self.server_signature = hmac_sha256(&server_key, message.as_bytes());
        Ok(format!("{without_proof},p={}", STANDARD.encode(proof)))
    }

    /// checks that the server also knows the password's verifier
    pub fn verify(&self, server_final: &[u8]) -> Result<()> {
        let text = std::str::from_utf8(server_final).map_err(|_| REFUSED)?;
        let signature = field(text.split(',').next(), "v=")?;
        let signature = STANDARD.decode(signature).map_err(|_| REFUSED)?;
        let mut difference = u8::from(signature.len() != 32);
        for (left, right) in signature.iter().zip(self.server_signature) {
            difference |= left ^ right;
        }
        if difference != 0 {
            return Err(Error::Authentication("server signature does not match"));
        }
        Ok(())
    }
}

fn field<'a>(part: Option<&'a str>, prefix: &str) -> Result<&'a str> {
    part.and_then(|part| part.strip_prefix(prefix))
        .ok_or(REFUSED)
}

pub(crate) fn nonce() -> String {
    let mut bytes = [0u8; 18];
    alt_os::random::fill(&mut bytes);
    STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "rOprNGfwEbeRWgbNEkqO";
    const SERVER_FIRST: &[u8] =
        b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096";

    fn exchange() -> Exchange {
        Exchange::new("user", "pencil", NONCE.into()).unwrap()
    }

    #[test]
    fn matches_the_rfc_7677_exchange() {
        let mut exchange = exchange();
        assert_eq!(exchange.client_first(), "n,,n=user,r=rOprNGfwEbeRWgbNEkqO");
        assert_eq!(
            exchange.client_final(SERVER_FIRST).unwrap(),
            "c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,\
             p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ="
        );
        exchange
            .verify(b"v=6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=")
            .unwrap();
        assert!(
            exchange
                .verify(b"v=7rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4=")
                .is_err()
        );
        assert!(exchange.verify(b"e=other-error").is_err());
        assert!(exchange.verify(b"v=6rri").is_err());
    }

    #[test]
    fn refuses_malformed_server_messages() {
        for server_first in [
            &b""[..],
            b"r=rOprNGfwEbeRWgbNEkqO,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            b"r=otherNonce%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            b"r=rOprNGfwEbeRWgbNEkqOx,s=not base64,i=4096",
            b"r=rOprNGfwEbeRWgbNEkqOx,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=0",
            b"r=rOprNGfwEbeRWgbNEkqOx,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4294967295",
            b"r=rOprNGfwEbeRWgbNEkqOx,s=W22ZaJ0SNY7soEsUEjb6gQ==",
            b"s=W22ZaJ0SNY7soEsUEjb6gQ==,r=rOprNGfwEbeRWgbNEkqOx,i=4096",
            b"r=rOprNGfwEbeRWgbNEkqOx\xff,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
        ] {
            assert!(exchange().client_final(server_first).is_err());
        }
    }

    #[test]
    fn passwords_outside_ascii_are_refused() {
        assert!(Exchange::new("", "pässword", NONCE.into()).is_err());
        assert_eq!(nonce().len(), 24);
        assert_ne!(nonce(), nonce());
    }
}
