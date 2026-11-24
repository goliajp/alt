//! URLs as the WHATWG URL Standard defines them: parsing, serialization,
//! resolution against a base, percent-encoding and form-urlencoded pairs.
//!
//! A [`Url`] is its serialization plus the offsets of its components, so
//! parsing allocates one string and the getters return slices of it.
//! Host names that are not ASCII after percent-decoding go through UTS #46
//! processing; an ASCII host name is lowercased and its `xn--` labels are
//! kept as written.

mod error;
pub mod form;
mod host;
mod idna;
mod ip;
mod origin;
mod parser;
pub mod percent;

pub use error::ParseError;
pub use host::Host;
pub use idna::UNICODE_VERSION;
pub use origin::Origin;

use host::HostKind;
use std::fmt;

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Url {
    serialization: String,
    /// offset of the `:` after the scheme
    scheme_end: usize,
    username_end: usize,
    host_start: usize,
    host_end: usize,
    host: HostKind,
    port: Option<u16>,
    path_start: usize,
    /// offset of the `?`
    query_start: Option<usize>,
    /// offset of the `#`
    fragment_start: Option<usize>,
}

impl Url {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        parser::parse(input, None)
    }

    /// Parses `input` with this URL as the base.
    pub fn join(&self, input: &str) -> Result<Self, ParseError> {
        parser::parse(input, Some(self))
    }

    pub fn as_str(&self) -> &str {
        &self.serialization
    }

    pub fn scheme(&self) -> &str {
        &self.serialization[..self.scheme_end]
    }

    pub fn has_authority(&self) -> bool {
        self.serialization[self.scheme_end + 1..].starts_with("//")
    }

    /// Whether the path is opaque, as in `mailto:` and `data:` URLs.
    pub fn cannot_be_a_base(&self) -> bool {
        !self.serialization[self.scheme_end + 1..].starts_with('/')
    }

    pub fn username(&self) -> &str {
        if self.has_authority() {
            &self.serialization[self.scheme_end + 3..self.username_end]
        } else {
            ""
        }
    }

    pub fn password(&self) -> Option<&str> {
        (self.host_start > self.username_end + 1)
            .then(|| &self.serialization[self.username_end + 1..self.host_start - 1])
    }

    pub fn host_str(&self) -> Option<&str> {
        (self.host != HostKind::None).then(|| &self.serialization[self.host_start..self.host_end])
    }

    pub fn host(&self) -> Option<Host<&str>> {
        match self.host {
            HostKind::None => None,
            HostKind::Domain => Some(Host::Domain(
                &self.serialization[self.host_start..self.host_end],
            )),
            HostKind::Ipv4(address) => Some(Host::Ipv4(address)),
            HostKind::Ipv6(address) => Some(Host::Ipv6(address)),
        }
    }

    /// The port, unless it is the default one of the scheme.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    pub fn port_or_known_default(&self) -> Option<u16> {
        self.port.or_else(|| parser::default_port(self.scheme()))
    }

    pub fn path(&self) -> &str {
        let end = self.query_start.or(self.fragment_start);
        &self.serialization[self.path_start..end.unwrap_or(self.serialization.len())]
    }

    pub fn query(&self) -> Option<&str> {
        let end = self.fragment_start.unwrap_or(self.serialization.len());
        self.query_start
            .map(|start| &self.serialization[start + 1..end])
    }

    pub fn fragment(&self) -> Option<&str> {
        self.fragment_start
            .map(|start| &self.serialization[start + 1..])
    }

    pub fn origin(&self) -> Origin {
        origin::of(self)
    }

    /// The name-value pairs of the query, form-urlencoded decoded.
    pub fn query_pairs(&self) -> form::Parse<'_> {
        form::parse(self.query().unwrap_or("").as_bytes())
    }

    /// Appends a form-urlencoded name-value pair to the query.
    pub fn append_query_pair(&mut self, name: &str, value: &str) -> &mut Self {
        let fragment = self
            .fragment_start
            .map(|start| self.serialization.split_off(start));
        match self.query_start {
            None => {
                self.query_start = Some(self.serialization.len());
                self.serialization.push('?');
            }
            Some(start) if start + 1 < self.serialization.len() => self.serialization.push('&'),
            Some(_) => {}
        }
        form::append_pair(&mut self.serialization, name, value);
        if let Some(fragment) = fragment {
            self.fragment_start = Some(self.serialization.len());
            self.serialization.push_str(&fragment);
        }
        self
    }
}

impl fmt::Debug for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Url").field(&self.serialization).finish()
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialization)
    }
}

impl std::str::FromStr for Url {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, ParseError> {
        Self::parse(input)
    }
}

impl AsRef<str> for Url {
    fn as_ref(&self) -> &str {
        &self.serialization
    }
}

impl From<Url> for String {
    fn from(url: Url) -> Self {
        url.serialization
    }
}
