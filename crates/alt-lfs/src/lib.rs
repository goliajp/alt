//! Git LFS, spec v1: the pointer files a repository commits in place of large
//! contents, and a client for the batch API that serves those contents.
//!
//! A pointer is a small text blob:
//!
//! ```text
//! version https://git-lfs.github.com/spec/v1
//! oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393
//! size 12345
//! ```

use std::collections::BTreeMap;
use std::io::Read;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SPEC: &str = "https://git-lfs.github.com/spec/v1";

/// Pointer files are small; git-lfs never treats anything larger as one.
pub const MAX_POINTER_BYTES: usize = 1024;

const MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("LFS request to {url} failed: {reason}")]
    Http { url: String, reason: String },
    #[error("LFS server answered with something that is not a batch response: {0}")]
    Response(String),
    #[error("LFS server has no {oid}: {message}")]
    Missing { oid: String, message: String },
    #[error("LFS object {oid} arrived corrupt: {reason}")]
    Corrupt { oid: String, reason: String },
    #[error("io")]
    Io(#[from] std::io::Error),
}

/// What a pointer names: the sha256 of the content (lowercase hex) and its
/// size in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pointer {
    pub oid: String,
    pub size: u64,
}

impl Pointer {
    /// The pointer for `content`.
    pub fn of(content: &[u8]) -> Pointer {
        Pointer {
            oid: hex(&Sha256::digest(content)),
            size: content.len() as u64,
        }
    }

    /// Reads a pointer file; `None` for anything else (keys out of order,
    /// a malformed oid, extra bytes, too large).
    pub fn parse(blob: &[u8]) -> Option<Pointer> {
        if blob.len() > MAX_POINTER_BYTES {
            return None;
        }
        let text = std::str::from_utf8(blob).ok()?;
        let mut lines = text.strip_suffix('\n')?.split('\n');
        if lines.next()? != format!("version {SPEC}") {
            return None;
        }
        let mut oid = None;
        let mut size = None;
        let mut last_key = "";
        for line in lines {
            let (key, value) = line.split_once(' ')?;
            // keys after version are sorted and unique
            if key <= last_key {
                return None;
            }
            last_key = key;
            match key {
                "oid" => {
                    let h = value.strip_prefix("sha256:")?;
                    let well_formed = h.len() == 64
                        && h.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                    oid = well_formed.then(|| h.to_owned());
                }
                "size" => size = value.parse().ok(),
                _ => {} // extensions are allowed and ignored
            }
        }
        Some(Pointer {
            oid: oid?,
            size: size?,
        })
    }

    /// The canonical pointer file.
    pub fn encode(&self) -> Vec<u8> {
        format!(
            "version {SPEC}\noid sha256:{}\nsize {}\n",
            self.oid, self.size
        )
        .into_bytes()
    }

    /// Whether `content` is what this pointer names.
    pub fn verify(&self, content: &[u8]) -> Result<(), Error> {
        let got = Pointer::of(content);
        if got != *self {
            return Err(Error::Corrupt {
                oid: self.oid.clone(),
                reason: format!("got sha256 {} and {} bytes", got.oid, got.size),
            });
        }
        Ok(())
    }
}

/// The LFS endpoint git-lfs derives from a remote's URL when nothing
/// overrides it: `<url>.git/info/lfs`, without doubling a `.git` suffix.
pub fn endpoint_for(remote_url: &str) -> String {
    let base = remote_url.trim_end_matches('/');
    if base.ends_with(".git") {
        format!("{base}/info/lfs")
    } else {
        format!("{base}.git/info/lfs")
    }
}

/// A client for one LFS endpoint.
pub struct Client {
    endpoint: String,
    agent: ureq::Agent,
    /// HTTP basic credentials, when the server wants them.
    auth: Option<(String, String)>,
}

#[derive(Serialize)]
struct BatchRequest<'a> {
    operation: &'a str,
    transfers: [&'a str; 1],
    objects: Vec<BatchObject<'a>>,
}

#[derive(Serialize)]
struct BatchObject<'a> {
    oid: &'a str,
    size: u64,
}

#[derive(Deserialize)]
struct BatchResponse {
    objects: Vec<ResponseObject>,
}

#[derive(Deserialize)]
struct ResponseObject {
    oid: String,
    #[serde(default)]
    actions: Option<Actions>,
    #[serde(default)]
    error: Option<ObjectError>,
}

#[derive(Deserialize)]
struct Actions {
    download: Option<Action>,
}

#[derive(Deserialize)]
struct Action {
    href: String,
    #[serde(default)]
    header: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ObjectError {
    message: String,
}

impl Client {
    pub fn new(endpoint: impl Into<String>, auth: Option<(String, String)>) -> Client {
        Client {
            endpoint: endpoint.into().trim_end_matches('/').to_owned(),
            agent: ureq::AgentBuilder::new().build(),
            auth,
        }
    }

    /// Downloads the contents `pointers` name, each checked against its
    /// pointer. One batch request asks for all of them; the contents then
    /// come from wherever the server points.
    pub fn download(&self, pointers: &[Pointer]) -> Result<Vec<(Pointer, Vec<u8>)>, Error> {
        if pointers.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!("{}/objects/batch", self.endpoint);
        let body = serde_json::to_string(&BatchRequest {
            operation: "download",
            transfers: ["basic"],
            objects: pointers
                .iter()
                .map(|p| BatchObject {
                    oid: &p.oid,
                    size: p.size,
                })
                .collect(),
        })
        .expect("a batch request serializes");
        let mut req = self
            .agent
            .post(&url)
            .set("Accept", MEDIA_TYPE)
            .set("Content-Type", MEDIA_TYPE);
        if let Some((user, token)) = &self.auth {
            req = req.set("Authorization", &basic(user, token));
        }
        let resp = req.send_string(&body).map_err(|e| http(&url, e))?;
        let text = resp.into_string()?;
        let batch: BatchResponse =
            serde_json::from_str(&text).map_err(|e| Error::Response(e.to_string()))?;

        let mut out = Vec::with_capacity(pointers.len());
        for p in pointers {
            let obj = batch
                .objects
                .iter()
                .find(|o| o.oid == p.oid)
                .ok_or_else(|| Error::Missing {
                    oid: p.oid.clone(),
                    message: "not in the batch response".into(),
                })?;
            if let Some(e) = &obj.error {
                return Err(Error::Missing {
                    oid: p.oid.clone(),
                    message: e.message.clone(),
                });
            }
            let action = obj
                .actions
                .as_ref()
                .and_then(|a| a.download.as_ref())
                .ok_or_else(|| Error::Missing {
                    oid: p.oid.clone(),
                    message: "no download action".into(),
                })?;
            let mut get = self.agent.get(&action.href);
            for (k, v) in &action.header {
                get = get.set(k, v);
            }
            let resp = get.call().map_err(|e| http(&action.href, e))?;
            let mut content = Vec::with_capacity(p.size as usize);
            resp.into_reader().read_to_end(&mut content)?;
            p.verify(&content)?;
            out.push((p.clone(), content));
        }
        Ok(out)
    }
}

fn http(url: &str, e: ureq::Error) -> Error {
    Error::Http {
        url: url.to_owned(),
        reason: match e {
            ureq::Error::Status(code, _) => format!("status {code}"),
            ureq::Error::Transport(t) => t.to_string(),
        },
    }
}

fn basic(user: &str, token: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let raw = format!("{user}:{token}").into_bytes();
    let mut out = String::from("Basic ");
    for chunk in raw.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn pointers_round_trip_and_name_their_content() {
        let p = Pointer::of(b"");
        assert_eq!(p.oid, EMPTY_SHA);
        assert_eq!(Pointer::parse(&p.encode()), Some(p.clone()));
        assert!(p.verify(b"").is_ok());
        assert!(p.verify(b"x").is_err());
    }

    #[test]
    fn only_well_formed_pointers_parse() {
        let good = format!("version {SPEC}\noid sha256:{EMPTY_SHA}\nsize 0\n");
        assert!(Pointer::parse(good.as_bytes()).is_some());
        // an extension key sorted between the others is fine
        let ext = format!(
            "version {SPEC}\next-0-foo sha256:{EMPTY_SHA}\noid sha256:{EMPTY_SHA}\nsize 0\n"
        );
        assert!(Pointer::parse(ext.as_bytes()).is_some());
        for bad in [
            format!("version {SPEC}\nsize 0\noid sha256:{EMPTY_SHA}\n"), // out of order
            format!("version {SPEC}\noid sha256:{}\nsize 0\n", &EMPTY_SHA[..63]), // short oid
            format!(
                "version {SPEC}\noid sha256:{}\nsize 0\n",
                EMPTY_SHA.to_uppercase()
            ),
            format!("version {SPEC}\noid sha256:{EMPTY_SHA}\nsize 0"), // no final newline
            format!("version other\noid sha256:{EMPTY_SHA}\nsize 0\n"),
            "just a file\n".to_owned(),
        ] {
            assert_eq!(Pointer::parse(bad.as_bytes()), None, "{bad:?}");
        }
    }

    #[test]
    fn endpoints_follow_git_lfs() {
        assert_eq!(endpoint_for("https://h/o/r"), "https://h/o/r.git/info/lfs");
        assert_eq!(
            endpoint_for("https://h/o/r.git"),
            "https://h/o/r.git/info/lfs"
        );
        assert_eq!(endpoint_for("https://h/o/r/"), "https://h/o/r.git/info/lfs");
    }

    #[test]
    fn basic_auth_encodes_like_base64() {
        assert_eq!(basic("a", "b"), "Basic YTpi");
        assert_eq!(basic("user", "pass"), "Basic dXNlcjpwYXNz");
    }
}
