//! The filters of a partial clone.

use alt_git_codec::ObjectKind;

/// The partial-clone filter shapes [`super::response`] currently
/// honours. Everything else parses as `None` (no filter) for
/// forward-compatibility — unknown filters degrade to a full pack
/// rather than rejecting the fetch, since the wire spec lets the
/// server be permissive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FilterSpec {
    pub(super) omit_blobs: bool,
    pub(super) blob_limit: Option<usize>,
    pub(super) omit_trees: bool,
}

impl FilterSpec {
    /// Whether an object of `kind` and of `bytes` bytes is left out.
    pub(super) fn excludes(&self, kind: ObjectKind, bytes: usize) -> bool {
        match kind {
            ObjectKind::Blob => {
                if self.omit_blobs || self.omit_trees {
                    return true;
                }
                if let Some(limit) = self.blob_limit
                    && bytes >= limit
                {
                    return true;
                }
                false
            }
            ObjectKind::Tree => self.omit_trees,
            _ => false,
        }
    }
}

pub(super) fn parse_filter_spec(raw: Option<&str>) -> FilterSpec {
    let mut out = FilterSpec::default();
    let Some(spec) = raw else {
        return out;
    };
    let spec = spec.trim();
    match spec {
        "blob:none" => out.omit_blobs = true,
        "tree:0" => {
            out.omit_trees = true;
            out.omit_blobs = true; // tree:0 implies blob:none semantically
        }
        _ => {
            if let Some(n) = spec.strip_prefix("blob:limit=")
                && let Ok(limit) = n.parse::<usize>()
            {
                out.blob_limit = Some(limit);
            }
            // unknown / unsupported filters silently degrade to "send
            // everything" — git's spec lets the server be permissive.
        }
    }
    out
}
