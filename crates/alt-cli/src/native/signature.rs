//! Who a commit, an annotated tag or a notes commit is written by: the
//! author and the committer, each named by the caller's environment and
//! then by the configuration, in the order git reads them. Nobody is made
//! up where neither names one.

use alt_git_config::Config;
use alt_worktree::Sig;

use super::{NativeRepo, Res, now_ms};

mod date;
#[cfg(test)]
mod test_zones;

/// The variables that name the author and the committer, and the zone a
/// date they give without one is in.
const SIGNING_ENV: &[&str] = &[
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "EMAIL",
    "TZ",
];

/// What the caller's environment says of the author and the committer,
/// by the names of [`SIGNING_ENV`].
#[derive(Clone, Default)]
pub struct Said(Vec<(&'static str, String)>);

impl Said {
    pub(super) fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let said = SIGNING_ENV.iter();
        Said(said.filter_map(|key| Some((*key, get(key)?))).collect())
    }

    fn get(&self, key: &str) -> Option<&str> {
        let said = self.0.iter().find(|(name, _)| *name == key)?;
        Some(&said.1)
    }

    pub(super) fn binding(&self) -> alt_json::Value {
        let said = self.0.iter();
        let said: std::collections::BTreeMap<String, String> = said
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect();
        alt_json::json!(said)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Role {
    Author,
    Committer,
}

impl Role {
    /// The word for it, the prefix of its variables, and its section of
    /// the configuration.
    fn names(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Role::Author => ("author", "GIT_AUTHOR", "author"),
            Role::Committer => ("committer", "GIT_COMMITTER", "committer"),
        }
    }
}

/// A name, an address and a time, as one line of a commit carries them.
pub(super) struct Signature {
    name: String,
    email: String,
    when: i64,
    tz: String,
}

impl Signature {
    pub(super) fn sig(&self) -> Sig<'_> {
        Sig {
            name: &self.name,
            email: &self.email,
            when: self.when,
            tz: &self.tz,
        }
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn email(&self) -> &str {
        &self.email
    }

    pub(super) fn when(&self) -> i64 {
        self.when
    }

    pub(super) fn tz(&self) -> &str {
        &self.tz
    }

    /// `Name <email> <seconds> <zone>`
    pub(super) fn line(&self) -> String {
        let Signature {
            name,
            email,
            when,
            tz,
        } = self;
        format!("{name} <{email}> {when} {tz}")
    }
}

/// Whether git drops `c` from either end of a name or an address.
fn crud(c: char) -> bool {
    c <= ' ' || matches!(c, ',' | ':' | ';' | '<' | '>' | '"' | '\\' | '\'')
}

/// `value` as git writes a name or an address: without what [`crud`] is
/// at its ends, and without any `<`, `>` or newline. One that has nothing
/// left names nobody.
pub(super) fn clean(value: &str) -> Option<String> {
    let kept = value.trim_matches(crud);
    let kept: String = kept
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '\n'))
        .collect();
    (!kept.is_empty()).then_some(kept)
}

fn configured(settings: &Config, section: &str, key: &str) -> Res<Option<String>> {
    let Some(value) = settings.get_str(section, None, key) else {
        return Ok(None);
    };
    let value = std::str::from_utf8(value)
        .map_err(|_| format!("the configured {section}.{key} is not UTF-8"))?;
    Ok(clean(value))
}

/// Ends the command that would have written the name or the address
/// nothing gives.
fn unknown(role: &str, name: bool, email: bool) -> Box<dyn std::error::Error> {
    let lacking = match (name, email) {
        (true, true) => "name and email are",
        (true, false) => "name is",
        _ => "email is",
    };
    let message = format!(
        "the {role}'s {lacking} not set, and nothing was written\n\n\
         Say who you are in this repository:\n\n  \
         alt config user.name \"Your Name\"\n  \
         alt config user.email \"you@example.com\"\n"
    );
    let refusal = crate::machine::Failure::refusal("precondition_failed", message);
    Box::new(crate::machine::Failure {
        exit_code: 128,
        ..refusal
    })
}

/// `instant` with the zone the caller is in then, as git writes the
/// moment a commit is made at: the seconds, and the zone as `±hhmm`.
fn in_zone(said: &Said, instant: i64) -> (i64, String) {
    let east = alt_os::zone::Zone::of(said.get("TZ")).offset_at(instant);
    (instant, date::zone(i64::from(east) / 60))
}

/// The moment `date` names, in the caller's zone where it names none.
fn read_date(said: &Said, date: &str, now: i64) -> Res<(i64, String)> {
    let local = |wall| alt_os::zone::Zone::of(said.get("TZ")).offset_of_local(wall);
    date::parse(date, now, &local)
        .ok_or_else(|| crate::git_settings::fatal(format!("invalid date format: {date}")))
}

fn resolve(said: &Said, role: Role, settings: &Config, now: i64) -> Res<Signature> {
    let (word, variables, own) = role.names();
    let told = |what: &str| said.get(&format!("{variables}_{what}"));
    let mut name = told("NAME").and_then(clean);
    let mut email = told("EMAIL").and_then(clean);
    for section in [own, "user"] {
        if name.is_none() {
            name = configured(settings, section, "name")?;
        }
        if email.is_none() {
            email = configured(settings, section, "email")?;
        }
    }
    let email = email.or_else(|| said.get("EMAIL").and_then(clean));
    let (name, email) = match (name, email) {
        (Some(name), Some(email)) => (name, email),
        (name, email) => return Err(unknown(word, name.is_none(), email.is_none())),
    };
    let (when, tz) = match told("DATE").filter(|date| !date.is_empty()) {
        Some(date) => read_date(said, date, now)?,
        None => in_zone(said, now),
    };
    Ok(Signature {
        name,
        email,
        when,
        tz,
    })
}

impl NativeRepo<'_> {
    /// Whom a commit made now is by.
    pub(super) fn author(&self) -> Res<Signature> {
        self.signature(Role::Author)
    }

    /// Who writes a commit or a tag now.
    pub(super) fn committer(&self) -> Res<Signature> {
        self.signature(Role::Committer)
    }

    /// Ends a command that is to write commits of its own making where
    /// nothing names their author or their committer.
    pub(super) fn ensure_signing(&self) -> Res<()> {
        self.author().and(self.committer()).map(drop)
    }

    /// This moment in the caller's zone, as the end of an author's or a
    /// committer's line.
    pub(super) fn moment(&self) -> String {
        let (seconds, zone) = in_zone(&self.id.said, (now_ms() / 1000) as i64);
        format!("{seconds} {zone}")
    }

    /// The moment `date`, given on the command line, names.
    pub(super) fn date_said(&self, date: &str) -> Res<(i64, String)> {
        read_date(&self.id.said, date, (now_ms() / 1000) as i64)
    }

    /// When a commit made now is authored: the date the caller's
    /// environment gives, or this moment.
    pub(super) fn author_moment(&self) -> Res<(i64, String)> {
        let now = (now_ms() / 1000) as i64;
        match self.id.said.get("GIT_AUTHOR_DATE") {
            Some(date) if !date.is_empty() => read_date(&self.id.said, date, now),
            _ => Ok(in_zone(&self.id.said, now)),
        }
    }

    fn signature(&self, role: Role) -> Res<Signature> {
        let applying = self.applying()?;
        resolve(
            &self.id.said,
            role,
            &applying.config,
            (now_ms() / 1000) as i64,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a caller in UTC says, where `pairs` name no other zone.
    fn said(pairs: &[(&str, &str)]) -> Said {
        let pairs = [pairs, &[("TZ", "")]].concat();
        Said::from_lookup(|key| {
            let pair = pairs.iter().find(|(name, _)| *name == key)?;
            Some(pair.1.to_owned())
        })
    }

    #[test]
    fn a_moment_given_no_date_is_in_the_zone_of_the_caller_then() {
        let zones = test_zones::Zones::new();
        // 2005-01-07 and 2005-07-07, either side of summer time
        for (tz, instant, written) in [
            ("Asia/Tokyo", 1_105_135_993, "U <u@x> 1105135993 +0900"),
            (
                "America/New_York",
                1_105_135_993,
                "U <u@x> 1105135993 -0500",
            ),
            (
                "America/New_York",
                1_120_774_393,
                "U <u@x> 1120774393 -0400",
            ),
            ("Asia/Kathmandu", 1_120_774_393, "U <u@x> 1120774393 +0545"),
            (
                "EST5EDT,M3.2.0,M11.1.0",
                1_120_774_393,
                "U <u@x> 1120774393 -0400",
            ),
            ("UTC", 1_120_774_393, "U <u@x> 1120774393 +0000"),
            ("", 1_120_774_393, "U <u@x> 1120774393 +0000"),
        ] {
            for role in [Role::Author, Role::Committer] {
                let tz = zones.tz(tz);
                let signature = resolve(&said(&[("TZ", &tz)]), role, &settings(USER), instant);
                assert_eq!(signature.unwrap().line(), written, "{tz}");
            }
        }
    }

    fn settings(text: &str) -> Config {
        Config {
            entries: alt_git_config::parse_file(text.as_bytes()).unwrap(),
        }
    }

    fn line(pairs: &[(&str, &str)], role: Role, config: &str) -> Result<String, String> {
        resolve(&said(pairs), role, &settings(config), 7)
            .map(|signature| signature.line())
            .map_err(|error| error.to_string())
    }

    const USER: &str = "[user]\n\tname = U\n\temail = u@x\n";

    #[test]
    fn the_environment_stands_before_the_configuration_and_each_role_before_user() {
        assert_eq!(line(&[], Role::Author, USER).unwrap(), "U <u@x> 7 +0000");
        assert_eq!(line(&[], Role::Committer, USER).unwrap(), "U <u@x> 7 +0000");
        let roles =
            format!("{USER}[author]\n\tname = AN\n\temail = an@x\n[committer]\n\temail = cn@x\n");
        assert_eq!(
            line(&[], Role::Author, &roles).unwrap(),
            "AN <an@x> 7 +0000"
        );
        assert_eq!(
            line(&[], Role::Committer, &roles).unwrap(),
            "U <cn@x> 7 +0000"
        );
        let env = [
            ("GIT_AUTHOR_NAME", "EA"),
            ("GIT_COMMITTER_EMAIL", "ec@x"),
            ("EMAIL", "e@x"),
        ];
        assert_eq!(
            line(&env, Role::Author, &roles).unwrap(),
            "EA <an@x> 7 +0000"
        );
        assert_eq!(
            line(&env, Role::Committer, &roles).unwrap(),
            "U <ec@x> 7 +0000"
        );
        // the author's variables say nothing of the committer
        let author = [("GIT_AUTHOR_NAME", "EA"), ("GIT_AUTHOR_EMAIL", "ea@x")];
        assert_eq!(
            line(&author, Role::Committer, USER).unwrap(),
            "U <u@x> 7 +0000"
        );
    }

    #[test]
    fn email_of_the_environment_is_the_last_an_address_is_taken_from() {
        let named = "[user]\n\tname = U\n";
        let env = [("EMAIL", "e@x")];
        assert_eq!(line(&env, Role::Author, named).unwrap(), "U <e@x> 7 +0000");
        assert_eq!(
            line(&env, Role::Committer, named).unwrap(),
            "U <e@x> 7 +0000"
        );
        assert_eq!(line(&env, Role::Author, USER).unwrap(), "U <u@x> 7 +0000");
    }

    #[test]
    fn nobody_is_made_up() {
        let user = [("USER", "alice"), ("HOSTNAME", "host")];
        let error = line(&user, Role::Author, "").unwrap_err();
        assert!(
            error.starts_with("the author's name and email are not set"),
            "{error}"
        );
        assert!(
            error.contains("alt config user.name \"Your Name\""),
            "{error}"
        );
        assert!(
            error.contains("alt config user.email \"you@example.com\""),
            "{error}"
        );
        let error = line(&[], Role::Committer, "[user]\n\temail = u@x\n").unwrap_err();
        assert!(
            error.starts_with("the committer's name is not set"),
            "{error}"
        );
        let error = line(&[], Role::Author, "[user]\n\tname = U\n").unwrap_err();
        assert!(
            error.starts_with("the author's email is not set"),
            "{error}"
        );
    }

    #[test]
    fn a_value_with_nothing_in_it_is_not_set() {
        let blank = [("GIT_AUTHOR_NAME", " \t"), ("GIT_AUTHOR_EMAIL", "")];
        assert_eq!(line(&blank, Role::Author, USER).unwrap(), "U <u@x> 7 +0000");
        let empty = "[user]\n\tname = \"  \"\n\temail =\n";
        let error = line(&[], Role::Author, empty).unwrap_err();
        assert!(
            error.starts_with("the author's name and email are not set"),
            "{error}"
        );
        let only = [("GIT_AUTHOR_NAME", "<>"), ("GIT_AUTHOR_EMAIL", "a@x")];
        let error = line(&only, Role::Author, "").unwrap_err();
        assert!(error.starts_with("the author's name is not set"), "{error}");
    }

    /// The values and what git 2.56.1 wrote for them.
    #[test]
    fn names_and_addresses_are_cleaned_as_git_cleans_them() {
        for (given, written) in [
            (" <A <b> c>\nd>. ", "A b cd."),
            ("\"C,", "C"),
            ("'A\tB\\", "A\tB"),
            (":C:D;", "C:D"),
            (".A.", ".A."),
            (" <a@\nx>> ", "a@x"),
            ("a b@x", "a b@x"),
            (".c@x.", ".c@x."),
            ("c@x;", "c@x"),
        ] {
            assert_eq!(clean(given).as_deref(), Some(written), "{given:?}");
        }
        let config = "[user]\n\tname = \" U <V> \"\n\temail = \"  <u@x>  \"\n";
        assert_eq!(
            line(&[], Role::Author, config).unwrap(),
            "U V <u@x> 7 +0000"
        );
    }

    #[test]
    fn each_role_has_its_own_date() {
        let env = [
            ("GIT_AUTHOR_DATE", "1700000000 +0900"),
            ("GIT_COMMITTER_DATE", "2005-04-07T22:13:13+02:00"),
        ];
        assert_eq!(
            line(&env, Role::Author, USER).unwrap(),
            "U <u@x> 1700000000 +0900"
        );
        assert_eq!(
            line(&env, Role::Committer, USER).unwrap(),
            "U <u@x> 1112904793 +0200"
        );
        let unset = [("GIT_AUTHOR_DATE", "")];
        assert_eq!(line(&unset, Role::Author, USER).unwrap(), "U <u@x> 7 +0000");
        // blanks are a date that cannot be read, as they are to git
        for blank in [" ", "\t", " \n"] {
            let error = line(&[("GIT_AUTHOR_DATE", blank)], Role::Author, USER).unwrap_err();
            assert_eq!(error, format!("invalid date format: {blank}"));
        }
        let zones = test_zones::Zones::new();
        let new_york = zones.tz("America/New_York");
        let zoned = [
            ("GIT_AUTHOR_DATE", "2005-04-07 22:13:13"),
            ("GIT_COMMITTER_DATE", "Thu Apr  7 22:13:13 EDT 2005"),
            ("TZ", &new_york),
        ];
        assert_eq!(
            line(&zoned, Role::Author, USER).unwrap(),
            "U <u@x> 1112926393 -0400"
        );
        assert_eq!(
            line(&zoned, Role::Committer, USER).unwrap(),
            "U <u@x> 1112926393 -0400"
        );
        let bad = [("GIT_AUTHOR_DATE", "yesterday")];
        let error = line(&bad, Role::Author, USER).unwrap_err();
        assert_eq!(error, "invalid date format: yesterday");
    }
}
