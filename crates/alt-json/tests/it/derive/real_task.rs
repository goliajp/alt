//! Copies of the task records and of the change-link event body, the most
//! involved types that are stored and digested.

use std::collections::BTreeMap;

use alt_json::{Decoder, FromJson, ToJson, Writer};

use super::drive::exercise;
use super::support::{Arbitrary, Tally, types};
use crate::check::{from_serde, to_serde};
use crate::rng::Rng;

/// Arbitrary JSON carried inside a record: a serde tree on one side and
/// this crate's tree on the other.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnyJson(serde_json::Value);

impl ToJson for AnyJson {
    fn to_json(&self, w: &mut Writer) -> alt_json::Result<()> {
        w.value(&from_serde(&self.0))
    }
}

impl FromJson for AnyJson {
    fn from_json(d: &mut Decoder<'_>) -> alt_json::Result<Self> {
        Ok(AnyJson(to_serde(&d.value()?)))
    }
}

impl Arbitrary for AnyJson {
    fn arbitrary(rng: &mut Rng) -> Self {
        AnyJson(Arbitrary::arbitrary(rng))
    }
}

/// A counter written as a decimal string of at most twenty digits, with
/// hand-written implementations on both sides.
#[derive(Debug, Clone, Copy)]
pub struct Counter(pub u64);

fn counter(text: &str) -> Result<Counter, String> {
    if text.is_empty()
        || text.len() > 20
        || (text.len() > 1 && text.starts_with('0'))
        || !text.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("canonical decimal string required".to_owned());
    }
    text.parse().map(Counter).map_err(|e| format!("{e}"))
}

impl serde::Serialize for Counter {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Counter {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(d)?;
        counter(&value).map_err(serde::de::Error::custom)
    }
}

impl ToJson for Counter {
    fn to_json(&self, w: &mut Writer) -> alt_json::Result<()> {
        w.string(&self.0.to_string())
    }
}

impl FromJson for Counter {
    fn from_json(d: &mut Decoder<'_>) -> alt_json::Result<Self> {
        counter(&String::from_json(d)?).map_err(alt_json::Error::custom)
    }
}

impl Arbitrary for Counter {
    fn arbitrary(rng: &mut Rng) -> Self {
        Counter(Arbitrary::arbitrary(rng))
    }
}

types! {
    #[both(deny_unknown_fields)]
    struct Scope { repository: String, workspace: String, }

    #[both(deny_unknown_fields)]
    struct Baseline { head: String, tree: String, refs: BTreeMap<String, String>, }

    #[both(deny_unknown_fields)]
    struct InputRoot { path: String, purpose: String, }

    #[both(deny_unknown_fields)]
    struct Context { commands: Vec<Vec<String>>, cwd: String, validation: AnyJson, }

    #[both(deny_unknown_fields)]
    struct RepairSource { issue: String, sequence: Counter, selected_samples: Vec<String>, }

    #[both(deny_unknown_fields)]
    struct TaskSpec {
        purpose: String,
        authority: String,
        allowed_paths: Vec<String>,
        auxiliary: Vec<InputRoot>,
        exclusions: Vec<String>,
        validation: Context,
        #[both(default, skip_serializing_if = "Option::is_none")]
        repair_source: Option<Box<RepairSource>>,
    }

    #[both(deny_unknown_fields)]
    struct Task {
        schema_version: u32,
        scope: Scope,
        spec: TaskSpec,
        baseline: Baseline,
        target_expected: BTreeMap<String, String>,
        creator_request: String,
    }

    #[both(deny_unknown_fields)]
    struct Candidate {
        schema_version: u32,
        task: String,
        parent: Option<String>,
        tree: String,
        source_selection: String,
        manifest: String,
        target_expected: BTreeMap<String, String>,
        creator_request: String,
    }

    #[both(deny_unknown_fields)]
    struct Artifact { blake3: String, bytes: u64, }

    #[both(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    enum Termination { Exit { code: i32, }, Signal { number: i32, }, Timeout, Unknown, }

    #[both(rename_all = "snake_case")]
    enum Provenance { Reported, Observed, }

    #[both(deny_unknown_fields)]
    struct Evidence {
        schema_version: u32,
        candidate: String,
        manifest: String,
        before: Option<String>,
        after: Option<String>,
        runner: String,
        actual_context: Context,
        binaries: BTreeMap<String, Artifact>,
        started_unix_ns: u64,
        finished_unix_ns: u64,
        termination: Termination,
        stdout: Option<Artifact>,
        stderr: Option<Artifact>,
        artifacts: BTreeMap<String, Artifact>,
        provenance: Provenance,
        original_report: Option<Artifact>,
        creator_request: String,
    }

    #[both(rename_all = "snake_case")]
    enum TaskState { Abandoned, Integrated, }

    #[both(deny_unknown_fields)]
    struct Event {
        schema_version: u32,
        task: String,
        previous: Option<String>,
        state: TaskState,
        reason: String,
        creator_request: String,
    }

    #[both(deny_unknown_fields)]
    struct RemoteRun {
        destination: String,
        directory: String,
        max_wait_seconds: u64,
        sync_program: Artifact,
        sync_stdout: Artifact,
        sync_stderr: Artifact,
    }

    #[both(deny_unknown_fields)]
    struct RunAttempt {
        schema_version: u32,
        task: String,
        candidate: String,
        manifest: String,
        nonce: String,
        local_directory: String,
        remote: Option<RemoteRun>,
        observer_path: String,
        observer: Artifact,
        spec: Artifact,
        authentication_key: Artifact,
        creator_request: String,
    }

    #[both(deny_unknown_fields)]
    struct Review {
        schema_version: u32,
        task: String,
        candidate: String,
        evidence: String,
        baseline: String,
        target_expected: BTreeMap<String, String>,
        reviewer: String,
        reason: String,
        commit_message: String,
        changes: Artifact,
        creator_request: String,
    }

    #[both(deny_unknown_fields)]
    struct Integration {
        schema_version: u32,
        task: String,
        review: String,
        commit: String,
        target: String,
        before: String,
        notes_before: Option<String>,
        notes_after: String,
        creator_request: String,
    }

    #[both(deny_unknown_fields)]
    struct Release { application: String, release: String, environment: String, }

    #[both(deny_unknown_fields)]
    struct Mapping { release: Release, sequence: Counter, declared_by: String, }

    #[both(rename_all = "snake_case")]
    enum Refusal { TargetUnavailable, NotAuthorized, }

    #[both(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
    enum Body {
        Mapping(Mapping),
        Withdraw { previous_mapping_digest: String, reason: String, declared_by: String, },
        RepairRequest(RepairSource),
        TaskBound { repair_request_digest: String, task: String, publication_digest: String, },
    }

    #[both(tag = "state", content = "reason", rename_all = "snake_case", deny_unknown_fields)]
    enum Decision { Applied, Refused(Refusal), }

    #[both(deny_unknown_fields)]
    struct SignedEvent { sequence: Counter, body: Body, decision: Decision, signature: String, }
}

macro_rules! exercise_all {
    ($rounds:expr; $($ty:ty)*) => {{
        let mut total = Tally::default();
        $(
            let tally = exercise::<$ty>(0x7a5c ^ stringify!($ty).len() as u64, $rounds);
            tally.report(concat!("derive real task types, ", stringify!($ty)));
            total.add(&tally);
        )*
        total
    }};
}

#[test]
fn task_records() {
    exercise_all!(600; Scope Baseline Context TaskSpec Task Candidate Artifact Termination Provenance
        Evidence Event RemoteRun RunAttempt Review Integration)
        .report("derive real task types, task records in total");
}

#[test]
fn change_link_events() {
    exercise_all!(1500; Counter RepairSource Mapping Body Decision SignedEvent)
        .report("derive real task types, change-link events in total");
}
