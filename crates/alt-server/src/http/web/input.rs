//! Request bodies of the browser API.

use serde::Deserialize;

#[derive(Deserialize, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub(super) struct Login {
    pub(super) login_name: String,
    pub(super) password: String,
}

#[derive(Deserialize, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub(super) struct Empty {}

#[derive(Deserialize, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub(super) struct DeviceDecision {
    pub(super) user_code: String,
    pub(super) allow: bool,
}

#[derive(Deserialize, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub(super) struct NewUser {
    pub(super) login_name: String,
    pub(super) display_name: String,
    pub(super) password: String,
    pub(super) administrator: bool,
}

#[derive(Deserialize, alt_json::FromJson)]
#[serde(deny_unknown_fields)]
#[json(deny_unknown_fields)]
pub(super) struct UserState {
    pub(super) disabled: bool,
}

#[cfg(test)]
mod json_tests {
    use alt_json::json;
    use alt_json_testkit::drive::exercise_reader;
    use alt_json_testkit::fixtures::{check_recorded, record_reader, verify_reader};
    use alt_json_testkit::rng::Rng;
    use alt_json_testkit::support::Arbitrary;

    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");

    fn text(rng: &mut Rng) -> String {
        String::arbitrary(rng)
    }

    fn login(rng: &mut Rng) -> alt_json::Value {
        json!({"login_name": text(rng), "password": text(rng)})
    }

    fn empty(_: &mut Rng) -> alt_json::Value {
        json!({})
    }

    fn decision(rng: &mut Rng) -> alt_json::Value {
        json!({"user_code": text(rng), "allow": rng.chance(2)})
    }

    fn new_user(rng: &mut Rng) -> alt_json::Value {
        json!({
            "login_name": text(rng),
            "display_name": text(rng),
            "password": text(rng),
            "administrator": rng.chance(2),
        })
    }

    fn user_state(rng: &mut Rng) -> alt_json::Value {
        json!({"disabled": rng.chance(2)})
    }

    #[test]
    fn same_as_serde() {
        let mut total = exercise_reader::<Login>(0xd200, 300, login, |v| {
            format!("{:?}", (&v.login_name, &v.password))
        });
        total.add(&exercise_reader::<Empty>(0xd201, 300, empty, |_| {
            String::new()
        }));
        total.add(&exercise_reader::<DeviceDecision>(
            0xd202,
            300,
            decision,
            |v| format!("{:?}", (&v.user_code, v.allow)),
        ));
        total.add(&exercise_reader::<NewUser>(0xd203, 300, new_user, |v| {
            format!(
                "{:?}",
                (&v.login_name, &v.display_name, &v.password, v.administrator)
            )
        }));
        total.add(&exercise_reader::<UserState>(
            0xd204,
            300,
            user_state,
            |v| v.disabled.to_string(),
        ));
        total.report("browser request bodies, 5 types in total");
    }

    #[test]
    fn recorded_verdicts() {
        let stored = |name: &str, text: String| check_recorded(&format!("{DIR}/{name}.txt"), &text);
        stored("web-Login", record_reader::<Login>(0xd210, 15, login));
        stored("web-Empty", record_reader::<Empty>(0xd211, 15, empty));
        stored(
            "web-DeviceDecision",
            record_reader::<DeviceDecision>(0xd212, 15, decision),
        );
        stored(
            "web-NewUser",
            record_reader::<NewUser>(0xd213, 15, new_user),
        );
        stored(
            "web-UserState",
            record_reader::<UserState>(0xd214, 15, user_state),
        );
        verify_reader::<Login>(DIR, "web-Login");
        verify_reader::<Empty>(DIR, "web-Empty");
        verify_reader::<DeviceDecision>(DIR, "web-DeviceDecision");
        verify_reader::<NewUser>(DIR, "web-NewUser");
        verify_reader::<UserState>(DIR, "web-UserState");
    }
}
