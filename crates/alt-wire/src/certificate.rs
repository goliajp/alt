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
/// and that carries the repository pushed to, as the address it is sent to names
/// it, on a push.
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

/// The scheme and the host of `pushee`, where it is the repository a push request
/// with the path `path` is for: the path that `path` has before the service it
/// asks for.
fn server_of<'a>(pushee: &'a str, path: &str) -> Option<(&'a str, &'a str)> {
    let (scheme, rest) = pushee.split_once("://")?;
    let (named, at) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let path = path.split('?').next().unwrap_or(path);
    let repository = path.strip_suffix("/git-receive-pack")?;
    let same = at.trim_end_matches('/') == repository.trim_end_matches('/');
    (same && !named.is_empty()).then_some((scheme, named))
}

/// Whether `pushee` is the repository a request with the `Host` header `host` and
/// the path `path` is for: the same host, as the client named it, and the path
/// that `path` has before the service it asks for. The header is the client's
/// word, and a server that goes by it takes a push that was signed for another
/// server and is sent on with that server's name: see [`pushee_is_at`].
pub fn pushee_is(pushee: &str, host: &str, path: &str) -> bool {
    server_of(pushee, path).is_some_and(|(_, named)| named.eq_ignore_ascii_case(host.trim()))
}

/// Whether `pushee` is the repository a request with the path `path` is for on
/// the server that is reached at `origins`, each a scheme and a host as
/// [`origin`] reads them.
pub fn pushee_is_at(pushee: &str, origins: &[String], path: &str) -> bool {
    server_of(pushee, path).is_some_and(|(scheme, named)| {
        let at = format!("{scheme}://{named}").to_ascii_lowercase();
        origins.contains(&at)
    })
}

/// An address a server is told it is reached at, as certificates are compared
/// with it: `http` or `https`, a host, and a port where it is not the one of
/// the scheme; in lower case, and nothing behind it but a slash.
pub fn origin(told: &str) -> Option<String> {
    let (scheme, rest) = told.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "http" => ":80",
        "https" => ":443",
        _ => return None,
    };
    let host = rest.strip_suffix('/').unwrap_or(rest).to_ascii_lowercase();
    let host = host.strip_suffix(default).unwrap_or(&host);
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b".-:[]".contains(&byte);
    (!host.is_empty() && host.bytes().all(plain)).then(|| format!("{scheme}://{host}"))
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
    fn a_repository_is_the_one_a_request_is_for_by_its_host_and_its_path() {
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

    #[test]
    fn a_server_that_is_told_its_addresses_takes_a_certificate_for_one_of_them_only() {
        for (told, read) in [
            ("https://Alt.Example", Some("https://alt.example")),
            ("https://alt.example:443/", Some("https://alt.example")),
            ("http://alt.example:80", Some("http://alt.example")),
            ("https://alt.example:80", Some("https://alt.example:80")),
            (" http://[::1]:8080 ", Some("http://[::1]:8080")),
            ("alt.example", None),
            ("ssh://alt.example", None),
            ("https://", None),
            ("https://alt.example/team", None),
            ("https://alice@alt.example", None),
            ("https://alt.example?x", None),
        ] {
            assert_eq!(origin(told).as_deref(), read, "{told}");
        }
        let here = [
            origin("https://alt.example").unwrap(),
            origin("http://10.0.0.5:8090").unwrap(),
        ];
        let push = "/team/repo/git-receive-pack";
        for (pushee, is) in [
            ("https://alt.example/team/repo", true),
            ("https://ALT.example/team/repo", true),
            ("http://10.0.0.5:8090/team/repo", true),
            // the other scheme, another port, another server, another repository
            ("http://alt.example/team/repo", false),
            ("https://alt.example:8443/team/repo", false),
            ("https://other.example/team/repo", false),
            ("https://alt.example/team/other", false),
            ("alt.example/team/repo", false),
        ] {
            assert_eq!(pushee_is_at(pushee, &here, push), is, "{pushee}");
        }
        assert!(!pushee_is_at("https://alt.example/team/repo", &[], push));
    }
}
