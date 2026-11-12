use crate::{Host, Url};

/// The origin of a URL. Opaque origins carry no identity, so origins are
/// compared through [`Origin::ascii_serialization`] only when both are tuples.
#[derive(Clone, Debug)]
pub enum Origin {
    Opaque,
    Tuple(String, Host<String>, u16),
}

impl Origin {
    pub fn is_tuple(&self) -> bool {
        matches!(self, Self::Tuple(..))
    }

    pub fn ascii_serialization(&self) -> String {
        match self {
            Self::Opaque => "null".to_owned(),
            Self::Tuple(scheme, host, port) => {
                if crate::parser::default_port(scheme) == Some(*port) {
                    format!("{scheme}://{host}")
                } else {
                    format!("{scheme}://{host}:{port}")
                }
            }
        }
    }
}

pub(crate) fn of(url: &Url) -> Origin {
    match url.scheme() {
        "blob" => match Url::parse(url.path()) {
            Ok(inner) if matches!(inner.scheme(), "http" | "https") => of(&inner),
            _ => Origin::Opaque,
        },
        scheme @ ("ftp" | "http" | "https" | "ws" | "wss") => {
            match (url.host(), url.port_or_known_default()) {
                (Some(host), Some(port)) => Origin::Tuple(scheme.to_owned(), host.to_owned(), port),
                _ => Origin::Opaque,
            }
        }
        _ => Origin::Opaque,
    }
}
