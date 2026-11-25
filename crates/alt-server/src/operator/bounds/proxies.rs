//! The proxies in front of an instance that is migrated.
//!
//! A configuration from before `http.trusted_proxies` was a key names none,
//! and the server then takes every caller for the proxy its connection comes
//! from. Which proxies stand in front of the application is known to whatever
//! runs it, not to the configuration from before: the package tells them to
//! its application container in `ALT_TRUSTED_PROXIES`, and a migration writes
//! them into a configuration that names none.

use crate::operator::{Error, Result};

/// the addresses or networks a list of them names, with commas between
fn named(told: &str) -> Result<Vec<String>> {
    let named: Vec<String> = told.split(',').map(|one| one.trim().to_owned()).collect();
    if (named.iter()).any(|one| one.parse::<crate::Network>().is_err()) {
        return Err(Error::Config(
            "ALT_TRUSTED_PROXIES names what is no address and no network",
        ));
    }
    Ok(named)
}

/// What a migrated configuration names for its proxies: what it named
/// before, or what this process is told where that is nothing.
pub(super) fn to_write(before: &[String]) -> Result<Vec<String>> {
    match std::env::var("ALT_TRUSTED_PROXIES") {
        Ok(told) if before.is_empty() => named(&told),
        _ => Ok(before.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proxies_told_are_addresses_or_networks_with_commas_between() {
        assert_eq!(
            named("127.0.0.1,::1, 172.30.91.1").unwrap(),
            ["127.0.0.1", "::1", "172.30.91.1"]
        );
        assert_eq!(named("10.0.0.0/8").unwrap(), ["10.0.0.0/8"]);
        for told in ["", "127.0.0.1,", "edge", "127.0.0.1 ::1", "10.0.0.0/33"] {
            assert!(matches!(named(told), Err(Error::Config(_))), "{told:?}");
        }
    }
}
