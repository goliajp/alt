//! The `ident` attribute: `$Id$` in a stored file is checked out as
//! `$Id: <blob id> $`, and stored again as `$Id$`.

/// Where a keyword that starts at `text[0] == b'$'` ends: the index of its
/// closing `$`. `$Id$` and `$Id: … $` on one line are keywords.
fn keyword_end(text: &[u8]) -> Option<usize> {
    match text.get(1..4)? {
        b"Id$" => Some(3),
        b"Id:" => {
            let close = 4 + text[4..].iter().position(|&b| b == b'$')?;
            (!text[4..close].contains(&b'\n')).then_some(close)
        }
        _ => None,
    }
}

/// Working-tree content as it is stored: every expanded keyword collapsed
/// to `$Id$`. `None` when there is nothing to collapse.
pub fn to_store(content: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(content.len());
    let mut changed = false;
    let mut at = 0;
    while let Some(found) = content[at..].iter().position(|&b| b == b'$') {
        let dollar = at + found;
        out.extend_from_slice(&content[at..=dollar]);
        at = dollar + 1;
        if content[dollar..].starts_with(b"$Id:")
            && let Some(end) = keyword_end(&content[dollar..])
        {
            out.extend_from_slice(b"Id$");
            at = dollar + end + 1;
            changed = true;
        }
    }
    out.extend_from_slice(&content[at..]);
    changed.then_some(out)
}

/// Stored content as it is checked out: every keyword expanded with
/// `id`, the hex id of the stored blob. An expansion that holds spaces in
/// unexpected places is taken for another system's and kept. `None` when
/// the content has no keyword.
pub fn to_worktree(blob: &[u8], id: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(blob.len() + 64);
    let mut changed = false;
    let mut at = 0;
    while let Some(found) = blob[at..].iter().position(|&b| b == b'$') {
        let dollar = at + found;
        out.extend_from_slice(&blob[at..=dollar]);
        at = dollar + 1;
        let Some(end) = keyword_end(&blob[dollar..]) else {
            continue;
        };
        if end > 3 {
            // `$Id: xxx $`: a space before the last character of the
            // value means the value is not one of ours
            let value = &blob[dollar + 4..dollar + end];
            let foreign = value.iter().skip(1).position(|&b| b == b' ');
            if foreign.is_some_and(|space| space + 2 < value.len()) {
                continue;
            }
        }
        out.extend_from_slice(format!("Id: {id} $").as_bytes());
        at = dollar + end + 1;
        changed = true;
    }
    out.extend_from_slice(&blob[at..]);
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn keywords_expand_on_checkout_and_collapse_when_stored() {
        let stored = b"/* $Id$ */\ncode $Idle$ $ Id$\n";
        let out = to_worktree(stored, ID).unwrap();
        assert_eq!(
            out,
            format!("/* $Id: {ID} $ */\ncode $Idle$ $ Id$\n").into_bytes()
        );
        assert_eq!(to_store(&out).unwrap(), stored.to_vec());
        assert_eq!(to_store(stored), None);
        assert_eq!(to_worktree(b"no keyword $ here\n", ID), None);
    }

    #[test]
    fn an_expanded_keyword_in_the_store_is_replaced_unless_it_is_foreign() {
        let ours = b"$Id: 1111111111111111111111111111111111111111 $\n";
        assert_eq!(
            to_worktree(ours, ID).unwrap(),
            format!("$Id: {ID} $\n").into_bytes()
        );
        // spaces inside the value: some other system wrote it
        let foreign = b"$Id: file.c,v 1.2 2001/01/01 author Exp $\n";
        assert_eq!(to_worktree(foreign, ID), None);
        // stored either way, both collapse
        assert_eq!(to_store(foreign).unwrap(), b"$Id$\n".to_vec());
        // a keyword is on one line
        assert_eq!(to_store(b"$Id: split\nhere $\n"), None);
        assert_eq!(to_worktree(b"$Id: split\nhere $\n", ID), None);
        // several on a line
        assert_eq!(
            to_store(b"$Id: a $ and $Id: b $").unwrap(),
            b"$Id$ and $Id$".to_vec()
        );
    }
}
