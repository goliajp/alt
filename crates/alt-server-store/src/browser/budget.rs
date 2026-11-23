//! How many sign-in attempts a minute are let in: from one address, from the
//! network around it, for one login, and from everybody.

use crate::database::Transaction;
use crate::store::{integer, now};
use crate::{Error, Result};

/// The networks an attempt counts for: its address, and the wider one the address is
/// of. A host is given a whole /64, so every address of one is the same; an IPv4
/// address written as an IPv6 one is the IPv4 address it is.
fn networks(address: std::net::IpAddr) -> [std::net::IpAddr; 2] {
    use std::net::IpAddr::{V4, V6};
    let address = match address {
        V6(v6) => v6.to_ipv4_mapped().map_or(address, V4),
        v4 => v4,
    };
    match address {
        V4(v4) => [address, V4((u32::from(v4) >> 8 << 8).into())],
        // an address of the link the server is on stands for a host by
        // itself: every host of the link has the same first 64 bits
        V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => {
            [address, V6((u128::from(v6) >> 64 << 64).into())]
        }
        V6(v6) => {
            let bits = u128::from(v6);
            [V6((bits >> 64 << 64).into()), V6((bits >> 80 << 80).into())]
        }
    }
}

/// what one address and the /24 or /48 it is of may attempt in a minute
const ADDRESS_ATTEMPTS: i32 = 100;
const NETWORK_ATTEMPTS: i32 = 400;
/// What may be attempted for one login in a minute: from one address, and from
/// everywhere. The first is what holds a guesser back; were it the only number
/// and counted for the login alone, whoever knows a login could keep its owner
/// out by guessing at it. The second bounds the guessing of many addresses.
///
/// Between the two is what the network around an address may attempt for one
/// login. Whoever has a network has many addresses in it, a household some
/// hundreds of /64 and more: without this number ten of them would spend what
/// everywhere may attempt, and the owner of the login would be refused with them.
/// With it that takes five networks, each a /24 or a /48. The price is that the
/// owner is held to this number where the guesser is in the same network.
const LOGIN_ATTEMPTS: i32 = 10;
const LOGIN_NETWORK_ATTEMPTS: i32 = 20;
const LOGIN_EVERYWHERE: i32 = 100;
/// The attempts of everybody in a minute past which an address is held to
/// [`BUSY_ATTEMPTS`]. Nobody is refused for this number alone: whoever can reach
/// it from many addresses would otherwise refuse everyone else with it. It is what
/// the instance can check of passwords in a minute: one takes three tenths of a
/// second and two are checked at once, which is four hundred. Past it the attempts
/// outrun the checking, and each address is held to the few that one person makes.
const BUSY: i32 = 400;
const BUSY_ATTEMPTS: i32 = 5;

/// Where this refuses, the caller lets go of its transaction: committed, the attempt
/// would still count against what every network and login share.
pub(crate) fn login_budget(
    tx: &mut Transaction<'_>,
    login: &str,
    network: std::net::IpAddr,
) -> Result<()> {
    let time = integer(now(tx)?)?;
    let [address, wider] = networks(network);
    let key = |kind: &str, of: &str| format!("{kind}:{}", alt_browser_auth::digest(of));
    // A single stable global row serializes admission. Stale keys are removed here.
    let rows = [
        ("global".to_owned(), i32::MAX),
        (key("network", &address.to_string()), ADDRESS_ATTEMPTS),
        (key("wider", &wider.to_string()), NETWORK_ATTEMPTS),
        (
            key("login-at", &format!("{login}\n{address}")),
            LOGIN_ATTEMPTS,
        ),
        (
            key("login-in", &format!("{login}\n{wider}")),
            LOGIN_NETWORK_ATTEMPTS,
        ),
        (key("login", login), LOGIN_EVERYWHERE),
    ];
    let mut busy = false;
    for (index, (key, max)) in rows.into_iter().enumerate() {
        let row=tx.query_one("INSERT INTO alt_login_budgets(key,window_start,attempts) VALUES($1,$2,1) ON CONFLICT(key) DO UPDATE SET window_start=CASE WHEN alt_login_budgets.window_start<=$2-60000 THEN $2 ELSE alt_login_budgets.window_start END,attempts=CASE WHEN alt_login_budgets.window_start<=$2-60000 THEN 1 ELSE alt_login_budgets.attempts+1 END RETURNING attempts", &[&key,&time])?;
        let attempts = row.get::<_, i32>(0);
        busy |= index == 0 && attempts > BUSY;
        if attempts > max || (index == 1 && busy && attempts > BUSY_ATTEMPTS) {
            return Err(Error::Precondition(
                "login admission limit; retry after one minute",
            ));
        }
    }
    tx.execute(
        "DELETE FROM alt_login_budgets WHERE key<>'global' AND window_start<$1::bigint-60000",
        &[&time],
    )?;
    Ok(())
}
