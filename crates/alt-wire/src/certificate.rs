//! What a signed push is signed over.
//!
//! A signature over the ref updates alone is good wherever those updates are: sent
//! again to the same server, or by a server that was pushed to, to another that
//! trusts the same key. So the signer signs what Git has a pusher sign in a push
//! certificate: who pushes, the repository pushed to as the pusher names it, the
//! nonce that repository gave for this one push, and the updates.

use crate::RefUpdate;
use alt_git_codec::HashAlgo;

/// Capability a server names where it takes signatures over [`push_certificate`],
/// and that carries the repository pushed to, as [`pushee`] gives it, on a push.
pub const CAP_ALT_PUSHEE: &str = "alt-pushee";

/// The bytes a push signature signs: the head of a Git push certificate, an empty
/// line, and a line for each update, in the order of their bytes.
pub fn push_certificate(
    pusher: &str,
    pushee: &str,
    nonce: &str,
    updates: &[RefUpdate],
    algo: HashAlgo,
) -> Vec<u8> {
    let zero = crate::push::zero_oid(algo);
    let mut lines: Vec<String> = (updates.iter())
        .map(|update| {
            let (old, new) = (update.old.unwrap_or(zero), update.new.unwrap_or(zero));
            format!("{old} {new} {}\n", update.name)
        })
        .collect();
    lines.sort();
    let head =
        format!("certificate version 0.1\npusher {pusher}\npushee {pushee}\nnonce {nonce}\n\n");
    [head, lines.concat()].concat().into_bytes()
}

/// The address of a repository as a certificate names it: the URL that is pushed
/// to, without the name and password it may carry, without a query, and without
/// a slash at its end.
pub fn pushee(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let (scheme, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, url),
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let path = path.trim_end_matches('/');
    match scheme {
        Some(scheme) => format!("{scheme}://{host}{path}"),
        None => format!("{host}{path}"),
    }
}

/// Whether `pushee` is the repository a request with the `Host` header `host` and
/// the path `path` is for: the same host, as the client named it, and the path
/// that `path` has before the service it asks for.
pub fn pushee_is(pushee: &str, host: &str, path: &str) -> bool {
    let Some((_, rest)) = pushee.split_once("://") else {
        return false;
    };
    let (named, at) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let path = path.split('?').next().unwrap_or(path);
    let Some(repository) = path.strip_suffix("/git-receive-pack") else {
        return false;
    };
    !named.is_empty()
        && named.eq_ignore_ascii_case(host.trim())
        && at.trim_end_matches('/') == repository.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_certificate_names_the_pusher_the_repository_and_the_nonce_before_the_updates() {
        let oid = |byte: u8| alt_git_codec::ObjectId::from_bytes(HashAlgo::Sha1, &[byte; 20]);
        let updates = [
            RefUpdate {
                name: "refs/heads/main".into(),
                old: oid(1).ok(),
                new: oid(2).ok(),
            },
            RefUpdate {
                name: "refs/heads/a".into(),
                old: None,
                new: oid(3).ok(),
            },
        ];
        let made = push_certificate("alice", "https://h/r", "n1", &updates, HashAlgo::Sha1);
        let expected = format!(
            "certificate version 0.1\npusher alice\npushee https://h/r\nnonce n1\n\n{} {} refs/heads/a\n{} {} refs/heads/main\n",
            "0".repeat(40),
            "03".repeat(20),
            "01".repeat(20),
            "02".repeat(20),
        );
        assert_eq!(String::from_utf8(made).unwrap(), expected);
    }

    #[test]
    fn a_repository_is_named_without_what_signs_in_to_it_and_is_the_one_a_request_is_for() {
        assert_eq!(
            pushee("https://alice:secret@Host.example:8443/team/repo/?x=1"),
            "https://Host.example:8443/team/repo"
        );
        assert_eq!(pushee("http://127.0.0.1:4000/"), "http://127.0.0.1:4000");
        assert_eq!(pushee("http://h/r.git#frag"), "http://h/r.git");
        for (pushee, host, path, is) in [
            (
                "http://127.0.0.1:4000",
                "127.0.0.1:4000",
                "/git-receive-pack",
                true,
            ),
            (
                "https://host.example/team/repo",
                "Host.Example",
                "/team/repo/git-receive-pack",
                true,
            ),
            (
                "https://host.example/team/repo",
                "host.example",
                "/team/repo/git-receive-pack?x",
                true,
            ),
            // another repository of the server, another server, and no server
            (
                "https://host.example/team/repo",
                "host.example",
                "/team/other/git-receive-pack",
                false,
            ),
            (
                "https://host.example/team/repo",
                "host.example",
                "/git-receive-pack",
                false,
            ),
            (
                "https://host.example/team/repo",
                "other.example",
                "/team/repo/git-receive-pack",
                false,
            ),
            (
                "https://host.example:8443/team/repo",
                "host.example",
                "/team/repo/git-receive-pack",
                false,
            ),
            (
                "host.example/team/repo",
                "host.example",
                "/team/repo/git-receive-pack",
                false,
            ),
            (
                "https:///team/repo",
                "",
                "/team/repo/git-receive-pack",
                false,
            ),
            (
                "https://host.example/team/repo",
                "host.example",
                "/team/repo/info/refs",
                false,
            ),
        ] {
            assert_eq!(pushee_is(pushee, host, path), is, "{pushee} {host} {path}");
        }
    }
}
