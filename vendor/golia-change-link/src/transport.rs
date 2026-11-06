//! Operator-provisioned endpoint policy shared by synchronous and asynchronous
//! adapters. This module performs no DNS or HTTP. Adapters must resolve with a
//! deadline, validate *all* answers here, pin the returned addresses, disable
//! ambient proxies and redirects, and preserve TLS hostname verification.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AddressPolicy {
    PublicHttps,
    /// Only operator configuration may choose these exact peer addresses.
    /// An API caller, repository file or signed event cannot supply this policy.
    Pinned {
        addresses: Vec<IpAddr>,
        #[serde(default)]
        allow_plaintext: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub origin: String,
    pub policy: AddressPolicy,
}
fn unicast(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => !v.is_unspecified() && !v.is_multicast() && !v.is_broadcast(),
        IpAddr::V6(v) => !v.is_unspecified() && !v.is_multicast(),
    }
}
/// Conservative public ranges, matching Sentori's existing outbound policy.
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127)
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192
                    && (b == 168 || (b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99)))
                || (a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}
impl Target {
    pub fn url(&self) -> Result<Url> {
        if self.origin.len() > 2048 || self.origin.chars().any(char::is_control) {
            return Err(Error::Invalid("connector origin"));
        }
        let url = Url::parse(&self.origin).map_err(|_| Error::Invalid("connector origin"))?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || !matches!(url.scheme(), "https" | "http")
            || url.port_or_known_default().is_none_or(|port| port == 0)
        {
            return Err(Error::Invalid("connector origin"));
        }
        match &self.policy {
            AddressPolicy::PublicHttps if url.scheme() != "https" => {
                return Err(Error::Invalid("connector TLS required"));
            }
            AddressPolicy::Pinned {
                addresses,
                allow_plaintext,
            } => {
                let unique: std::collections::BTreeSet<_> = addresses.iter().collect();
                if addresses.is_empty()
                    || addresses.len() > 32
                    || unique.len() != addresses.len()
                    || addresses.iter().any(|ip| !unicast(*ip))
                    || (url.scheme() != "https" && !allow_plaintext)
                {
                    return Err(Error::Invalid("connector pinned addresses"));
                }
            }
            _ => (),
        }
        if let Some(url::Host::Ipv4(ip)) = url.host() {
            self.check_ip(IpAddr::V4(ip))?;
        }
        if let Some(url::Host::Ipv6(ip)) = url.host() {
            self.check_ip(IpAddr::V6(ip))?;
        }
        Ok(url)
    }
    fn check_ip(&self, ip: IpAddr) -> Result<()> {
        let allowed = match &self.policy {
            AddressPolicy::PublicHttps => public_ip(ip),
            AddressPolicy::Pinned { addresses, .. } => addresses.contains(&ip),
        };
        if !allowed {
            return Err(Error::Authority);
        }
        Ok(())
    }
    /// Reject mixed public/private DNS answers and unexpected ports before any
    /// connection. The transport uses these exact addresses, never re-resolves.
    pub fn resolved(&self, addresses: &[SocketAddr]) -> Result<Vec<SocketAddr>> {
        let url = self.url()?;
        let port = url
            .port_or_known_default()
            .ok_or(Error::Invalid("connector port"))?;
        if addresses.is_empty() || addresses.len() > 32 {
            return Err(Error::Limit);
        }
        for address in addresses {
            if address.port() != port {
                return Err(Error::Authority);
            }
            self.check_ip(address.ip())?;
            match url.host() {
                Some(url::Host::Ipv4(ip)) if address.ip() != IpAddr::V4(ip) => {
                    return Err(Error::Authority);
                }
                Some(url::Host::Ipv6(ip)) if address.ip() != IpAddr::V6(ip) => {
                    return Err(Error::Authority);
                }
                _ => (),
            }
        }
        Ok(addresses.to_vec())
    }
}
