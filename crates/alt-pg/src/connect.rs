//! Startup and authentication.

use crate::{
    Config, Error, Result,
    link::Link,
    scram::{self, Exchange},
    stream::{Inbox, Stream},
    wire::{self, Body},
};

const PROTOCOL_3_0: u32 = 196_608;

impl Link {
    pub fn open(config: &Config) -> Result<Self> {
        let user = config
            .user
            .as_deref()
            .ok_or(Error::Config("user missing"))?;
        let mut link = Self {
            stream: Stream::open(config)?,
            inbox: Inbox::new(),
            out: Vec::with_capacity(1024),
            closing: Vec::new(),
            status: b'I',
            begin: false,
            broken: false,
        };
        let out = &mut link.out;
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&PROTOCOL_3_0.to_be_bytes());
        let database = config.dbname.as_deref().unwrap_or(user);
        let mut parameters = vec![
            ("user", user),
            ("database", database),
            ("client_encoding", "UTF8"),
        ];
        if let Some(name) = &config.application_name {
            parameters.push(("application_name", name));
        }
        if let Some(options) = &config.options {
            parameters.push(("options", options));
        }
        for (name, value) in parameters {
            wire::cstring(out, name.as_bytes()).map_err(|_| Error::Config("NUL byte"))?;
            wire::cstring(out, value.as_bytes()).map_err(|_| Error::Config("NUL byte"))?;
        }
        out.push(0);
        wire::finish(out, 0)?;
        link.stream.send(&link.out)?;
        link.authenticate(config)?;
        Ok(link)
    }

    fn authenticate(&mut self, config: &Config) -> Result<()> {
        let mut sasl = Sasl::default();
        loop {
            let (tag, range) = self.inbox.next(&mut self.stream)?;
            let body = self.inbox.bytes(range);
            match tag {
                b'R' => {
                    self.out.clear();
                    sasl.answer(Body::new(body), config, &mut self.out)?;
                    if !self.out.is_empty() {
                        self.stream.send(&self.out)?;
                    }
                }
                b'E' => return Err(Error::Server(Box::new(wire::server_error(body)?))),
                // parameter status, backend key data, notice
                b'S' | b'K' | b'N' => {}
                b'Z' => {
                    let &[status @ (b'I' | b'T' | b'E')] = body else {
                        return Err(Error::Protocol("transaction status"));
                    };
                    self.status = status;
                    self.inbox.relax();
                    return Ok(());
                }
                _ => return Err(Error::Protocol("unexpected message during startup")),
            }
        }
    }
}

#[derive(Default)]
struct Sasl {
    exchange: Option<Exchange>,
    verified: bool,
}

impl Sasl {
    /// handles one authentication request, writing the reply it calls for
    fn answer(&mut self, mut body: Body<'_>, config: &Config, out: &mut Vec<u8>) -> Result<()> {
        match body.i32()? {
            0 => {
                body.end()?;
                if self.exchange.is_some() && !self.verified {
                    return Err(Error::Authentication("server skipped its SCRAM signature"));
                }
            }
            10 => {
                let mut offered = false;
                loop {
                    let mechanism = body.cstr()?;
                    if mechanism.is_empty() {
                        break;
                    }
                    offered |= mechanism == scram::MECHANISM.as_bytes();
                }
                body.end()?;
                if !offered || self.exchange.is_some() {
                    return Err(Error::Authentication(
                        "server offers no supported SASL mechanism",
                    ));
                }
                let password = config
                    .password
                    .as_deref()
                    .ok_or(Error::Authentication("password missing"))?;
                // the server takes the user from the startup message
                let exchange = Exchange::new("", password, scram::nonce())?;
                let first = exchange.client_first();
                let at = wire::begin(out, b'p');
                wire::cstring(out, scram::MECHANISM.as_bytes())?;
                out.extend_from_slice(&(first.len() as u32).to_be_bytes());
                out.extend_from_slice(first.as_bytes());
                wire::finish(out, at)?;
                self.exchange = Some(exchange);
            }
            11 => {
                let exchange = self
                    .exchange
                    .as_mut()
                    .ok_or(Error::Protocol("SASL continuation without a start"))?;
                let last = exchange.client_final(body.rest())?;
                let at = wire::begin(out, b'p');
                out.extend_from_slice(last.as_bytes());
                wire::finish(out, at)?;
            }
            12 => {
                self.exchange
                    .as_ref()
                    .ok_or(Error::Protocol("SASL completion without a start"))?
                    .verify(body.rest())?;
                self.verified = true;
            }
            _ => {
                return Err(Error::Authentication(
                    "server requires an unsupported authentication method",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(password: Option<&str>) -> Config {
        let mut config = Config::default();
        if let Some(password) = password {
            config.set_password(password);
        }
        config
    }

    #[test]
    fn authentication_requests_are_checked() {
        let mut out = Vec::new();
        Sasl::default()
            .answer(Body::new(&[0, 0, 0, 0]), &config(None), &mut out)
            .unwrap();
        assert!(out.is_empty());
        let offer = b"\0\0\0\x0aSCRAM-SHA-256-PLUS\0SCRAM-SHA-256\0\0";
        let mut sasl = Sasl::default();
        sasl.answer(Body::new(offer), &config(Some("secret")), &mut out)
            .unwrap();
        assert!(out.starts_with(b"p\0\0\0\x36SCRAM-SHA-256\0\0\0\0\x20n,,n=,r="));
        // success before the server proved it knows the password is refused
        assert!(matches!(
            sasl.answer(Body::new(&[0, 0, 0, 0]), &config(Some("secret")), &mut out),
            Err(Error::Authentication(_))
        ));
        for (request, password) in [
            (&offer[..], None),
            (b"\0\0\0\x0aSCRAM-SHA-1\0\0", Some("secret")),
            (b"\0\0\0\x0aSCRAM-SHA-256\0", Some("secret")),
            (b"\0\0\0\x0aSCRAM-SHA-256", Some("secret")),
            (b"\0\0\0\x03", Some("secret")),
            (b"\0\0\0\x05abcd", Some("secret")),
            (b"\0\0\0\x0br=x", Some("secret")),
            (b"\0\0\0\x0cv=x", Some("secret")),
            (b"\0\0", Some("secret")),
            (b"\0\0\0\0\0", Some("secret")),
        ] {
            let result = Sasl::default().answer(Body::new(request), &config(password), &mut out);
            assert!(result.is_err(), "{request:?}");
        }
    }
}
