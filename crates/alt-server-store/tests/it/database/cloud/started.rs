//! The sign-ins through the platform that are begun, and what is counted of them.

use super::*;

/// A sign-in through the platform is begun without a credential, and no password
/// is checked for it here: those begun are held to what an address and its
/// network may begin, and take nothing from what everybody attempts of sign-ins.
#[test]
#[ignore = "requires isolated PostgreSQL; the Cloud sign-ins begun and the sign-ins of everybody"]
fn cloud_sign_ins_begun_take_nothing_from_what_is_attempted_of_sign_ins() {
    let mut f = cloud();
    let mut begun = 0u32;
    let mut begin = |f: &mut Fixture, network: &str| {
        begun += 1;
        let id = format!("{begun:064x}");
        let token = format!("alt1_{begun:064x}");
        let digest = alt_server_store::cloud_credential_digest(&token).unwrap();
        f.store
            .begin_cloud_browser_login(&id, &digest, 3_600_000, network.parse().unwrap())
    };
    let attempts = |f: &Fixture, key: &str| -> i32 {
        let counted = "SELECT COALESCE((SELECT attempts FROM alt_login_budgets WHERE key=$1),0)";
        f.client().query_one(counted, &[&key]).unwrap().get(0)
    };
    let of = |kind: &str, network: &str| format!("{kind}:{}", alt_browser_auth::digest(network));
    // more than everybody may attempt of sign-ins before an address is held to
    // a few, from five addresses of two networks
    for asked in 0..420 {
        let network = format!("198.51.{}.{}", 100 + asked % 2, 1 + asked % 5);
        begin(&mut f, &network).unwrap();
    }
    assert_eq!(attempts(&f, "global"), 0);
    assert_eq!(attempts(&f, &of("start", "198.51.100.1")), 42);

    // an address is held to what it may begin, and so is its network
    let set = "UPDATE alt_login_budgets SET attempts=$2 WHERE key=$1";
    let held = |f: &Fixture, key: &str, to: i32| {
        assert_eq!(f.client().execute(set, &[&key, &to]).unwrap(), 1);
    };
    held(&f, &of("start", "198.51.100.1"), 100);
    match begin(&mut f, "198.51.100.1") {
        Err(alt_server_store::Error::Attempts(left)) => {
            assert!((1..=60_000).contains(&left), "{left}")
        }
        other => panic!("{other:?}"),
    }
    begin(&mut f, "198.51.100.3").unwrap();
    held(&f, &of("start-wider", "198.51.100.0"), 400);
    assert!(begin(&mut f, "198.51.100.3").is_err());
    begin(&mut f, "198.51.101.3").unwrap();
}
