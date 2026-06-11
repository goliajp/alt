use alt_git_codec::{HashAlgo, ObjectId};

use crate::{Ref, RefError, RefTarget};

/// Parses a `packed-refs` file: `#`-comment header, `<hex> <name>` lines,
/// and `^<hex>` peeled lines attaching to the preceding (tag) ref.
pub(crate) fn parse(data: &[u8], algo: HashAlgo) -> Result<Vec<Ref>, RefError> {
    let mut out: Vec<Ref> = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        if line[0] == b'^' {
            let oid = parse_oid(&line[1..], algo)?;
            let last = out
                .last_mut()
                .ok_or(RefError::Format("peeled line before any ref"))?;
            last.peeled = Some(oid);
            continue;
        }
        let space = line
            .iter()
            .position(|&b| b == b' ')
            .ok_or(RefError::Format("packed-refs line without space"))?;
        let oid = parse_oid(&line[..space], algo)?;
        out.push(Ref {
            name: line[space + 1..].into(),
            target: RefTarget::Direct(oid),
            peeled: None,
        });
    }
    Ok(out)
}

pub(crate) fn parse_oid(hex: &[u8], algo: HashAlgo) -> Result<ObjectId, RefError> {
    let oid = ObjectId::from_hex(hex).map_err(|_| RefError::Format("invalid object id in ref"))?;
    if oid.algo() != algo {
        return Err(RefError::Format("ref oid length does not match repo algo"));
    }
    Ok(oid)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"# pack-refs with: peeled fully-peeled sorted \n\
        3b18e512dba79e4c8300dd08aeb37f8e728b8dad refs/heads/main\n\
        4b825dc642cb6eb9a060e54bf8d69288fbee4904 refs/tags/v0\n\
        ^e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n";

    #[test]
    fn parses_refs_and_peeled_lines() {
        let refs = parse(SAMPLE, HashAlgo::Sha1).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name, "refs/heads/main");
        assert!(refs[0].peeled.is_none());
        assert_eq!(refs[1].name, "refs/tags/v0");
        assert_eq!(
            refs[1].peeled.unwrap().to_string(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(
            parse(
                b"^e69de29bb2d1d6434b8b29ae775ad8c2e48c5391\n",
                HashAlgo::Sha1
            )
            .is_err()
        );
        assert!(parse(b"deadbeef refs/heads/x\n", HashAlgo::Sha1).is_err());
        assert!(
            parse(
                b"3b18e512dba79e4c8300dd08aeb37f8e728b8dad\n",
                HashAlgo::Sha1
            )
            .is_err()
        );
        // sha256-length oid in a sha1 repo
        let mixed =
            b"473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813 refs/heads/x\n";
        assert!(parse(mixed, HashAlgo::Sha1).is_err());
    }
}
