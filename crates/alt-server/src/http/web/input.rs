//! Request bodies of the browser API.

#[derive(alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct Login {
    pub(super) login_name: String,
    pub(super) password: String,
}

#[derive(alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct Empty {}

#[derive(alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct DeviceDecision {
    pub(super) user_code: String,
    pub(super) allow: bool,
}

#[derive(alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct NewUser {
    pub(super) login_name: String,
    pub(super) display_name: String,
    pub(super) password: String,
    pub(super) administrator: bool,
}

#[derive(alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct UserState {
    pub(super) disabled: bool,
}

#[cfg(test)]
mod json_tests {
    use alt_json_testkit::fixtures::verify_reader;

    use super::*;

    #[test]
    fn recorded_verdicts() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");
        verify_reader::<Login>(dir, "web-Login");
        verify_reader::<Empty>(dir, "web-Empty");
        verify_reader::<DeviceDecision>(dir, "web-DeviceDecision");
        verify_reader::<NewUser>(dir, "web-NewUser");
        verify_reader::<UserState>(dir, "web-UserState");
    }
}
