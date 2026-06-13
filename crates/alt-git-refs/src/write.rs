//! Writing a files-backend ref layout: loose ref files plus HEAD. The
//! export path uses this to rebuild a `.git` from native state; loose
//! files are the universally readable form (packed-refs and reftable are
//! optimizations git applies itself later).

use std::fs;
use std::path::Path;

use bstr::{BString, ByteSlice};

use crate::{RefError, RefTarget};

/// Writes `HEAD` and every ref as loose files under `git_dir`. Ref names
/// come from external state, so path-shaped abuse ("..", absolute,
/// empty components) is rejected at this trust boundary.
pub fn write_loose(
    git_dir: &Path,
    head: Option<&RefTarget>,
    refs: &[(BString, RefTarget)],
) -> Result<(), RefError> {
    if let Some(head) = head {
        fs::write(git_dir.join("HEAD"), render(head))?;
    }
    for (name, target) in refs {
        let name = name
            .to_str()
            .map_err(|_| RefError::Format("ref name is not utf-8"))?;
        validate_name(name)?;
        let path = git_dir.join(name);
        let parent = path.parent().ok_or(RefError::Format("bad ref name"))?;
        fs::create_dir_all(parent)?;
        fs::write(&path, render(target))?;
    }
    Ok(())
}

fn render(target: &RefTarget) -> Vec<u8> {
    match target {
        RefTarget::Direct(oid) => format!("{oid}\n").into_bytes(),
        RefTarget::Symbolic(name) => {
            let mut out = b"ref: ".to_vec();
            out.extend_from_slice(name);
            out.push(b'\n');
            out
        }
    }
}

fn validate_name(name: &str) -> Result<(), RefError> {
    let ok = !name.is_empty()
        && !name.starts_with('/')
        && !name.ends_with('/')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if ok {
        Ok(())
    } else {
        Err(RefError::Format("ref name fails path safety"))
    }
}
