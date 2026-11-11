//! Copies of the wire types of the remote protocol: the ones whose bytes
//! are signed or digested. Definitions follow the originals field for field.

use super::drive::exercise;
use super::shapes::decimal;
use super::support::{Tally, types};

types! {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    #[both(rename_all = "snake_case")]
    enum ObjectFormat { Sha1, Sha256, }

    #[both(deny_unknown_fields)]
    struct Scope {
        installation_id: String,
        repository_instance_id: String,
        #[both(with = "decimal")]
        recovery_epoch: u64,
    }

    #[both(deny_unknown_fields)]
    struct RequestKey { scope: Scope, actor_id: String, request_id: String, }

    #[both(deny_unknown_fields)]
    struct Limits {
        request_bytes: usize,
        ref_updates: usize,
        record_roots: usize,
        manifest_entries: usize,
        graph_objects: usize,
        graph_edges: usize,
        structured_bytes: usize,
        tree_paths: usize,
        path_bytes: usize,
        walk_steps: usize,
        #[both(with = "decimal")]
        chunk_bytes: u64,
        #[both(with = "decimal")]
        object_bytes: u64,
        #[both(with = "decimal")]
        total_bytes: u64,
    }

    #[both(rename_all = "snake_case")]
    enum ActorKind { Human, Agent, Service, }

    #[both(deny_unknown_fields)]
    struct Attribution {
        kind: ActorKind,
        on_behalf_of: Option<String>,
        delegation_id: Option<String>,
        agent_session: Option<String>,
    }

    #[both(rename_all = "snake_case")]
    enum DeploymentMode { SelfHosted, Cloud, }

    #[both(rename_all = "snake_case")]
    enum Feature { GitSmartHttp, Publications, TaskRecords, TaskBundlesV1, Receipts, PortableBundles, }

    #[both(deny_unknown_fields)]
    struct Capabilities {
        protocol_min: u32,
        protocol_max: u32,
        installation_id: String,
        #[both(with = "decimal")]
        runtime_generation: u64,
        deployment_mode: DeploymentMode,
        object_formats: Vec<ObjectFormat>,
        features: Vec<Feature>,
        limits: Limits,
    }

    #[both(deny_unknown_fields)]
    struct RefUpdate { name: String, expected: Option<String>, new: Option<String>, force: bool, }

    #[both(rename_all = "snake_case")]
    enum RecordKind { Task, Candidate, Evidence, Review, Integration, Delivery, }

    #[both(deny_unknown_fields)]
    struct RecordRoot { kind: RecordKind, id: String, }

    #[both(deny_unknown_fields)]
    struct Publication {
        protocol_version: u32,
        key: RequestKey,
        attribution: Attribution,
        object_format: ObjectFormat,
        refs: Vec<RefUpdate>,
        record_roots: Vec<RecordRoot>,
        content_manifest: String,
    }

    #[both(rename_all = "snake_case")]
    enum ContentKind { Commit, Tree, Blob, Tag, Record, Attachment, TaskBundle, }

    #[both(deny_unknown_fields)]
    struct Chunk {
        #[both(with = "decimal")]
        offset: u64,
        #[both(with = "decimal")]
        bytes: u64,
        blake3: String,
    }

    #[both(deny_unknown_fields)]
    struct ContentEntry {
        kind: ContentKind,
        blake3: String,
        #[both(with = "decimal")]
        bytes: u64,
        git_oid: Option<String>,
        chunks: Vec<Chunk>,
    }

    #[both(deny_unknown_fields)]
    struct Manifest { protocol_version: u32, object_format: ObjectFormat, entries: Vec<ContentEntry>, }

    #[both(rename_all = "snake_case")]
    enum ErrorCode {
        Unauthenticated,
        Forbidden,
        PreconditionFailed,
        UnsupportedVersion,
        QuotaExceeded,
        IntegrityError,
        TemporarilyUnavailable,
        OutcomeUnknown,
        InvalidRequest,
        RequestIdConflict,
    }

    #[both(rename_all = "snake_case")]
    enum Retry { Never, SameRequest, QueryOutcome, NewIntent, }

    #[both(deny_unknown_fields)]
    struct Failure { code: ErrorCode, message: String, request: Option<RequestKey>, retry: Retry, }

    #[both(deny_unknown_fields)]
    struct ReceiptBody {
        publication: Publication,
        request_digest: String,
        #[both(with = "decimal")]
        transaction_sequence: u64,
        #[both(with = "decimal")]
        policy_version: u64,
        #[both(with = "decimal")]
        committed_at_unix_ms: u64,
    }

    #[both(deny_unknown_fields)]
    struct Receipt { body: ReceiptBody, signing_key_id: String, signature: String, }
}

macro_rules! exercise_all {
    ($rounds:expr; $($ty:ty)*) => {{
        let mut total = Tally::default();
        $(
            let tally = exercise::<$ty>(0x9e0 ^ stringify!($ty).len() as u64, $rounds);
            tally.report(concat!("derive real protocol types, ", stringify!($ty)));
            total.add(&tally);
        )*
        total
    }};
}

#[test]
fn small_types() {
    exercise_all!(1000; ObjectFormat Scope RequestKey Limits ActorKind Attribution DeploymentMode Feature
        RefUpdate RecordKind RecordRoot ContentKind Chunk ErrorCode Retry Failure)
        .report("derive real protocol types, small types in total");
}

#[test]
fn signed_and_digested_types() {
    exercise_all!(1000; Capabilities Publication ContentEntry Manifest ReceiptBody Receipt)
        .report("derive real protocol types, signed and digested types in total");
}

/// the bytes a signature covers, pinned without the reference
#[test]
fn signing_bytes_are_pinned() {
    let scope = Scope {
        installation_id: "i".into(),
        repository_instance_id: "r".into(),
        recovery_epoch: u64::MAX,
    };
    let chunk = Chunk {
        offset: 0,
        bytes: 1 << 53,
        blake3: "aa".into(),
    };
    assert_eq!(
        alt_json::to_string(&scope).unwrap(),
        r#"{"installation_id":"i","repository_instance_id":"r","recovery_epoch":"18446744073709551615"}"#
    );
    assert_eq!(
        alt_json::to_string(&chunk).unwrap(),
        r#"{"offset":"0","bytes":"9007199254740992","blake3":"aa"}"#
    );
    assert!(alt_json::from_str::<Chunk>(r#"{"offset":"00","bytes":"1","blake3":""}"#).is_err());
    assert!(alt_json::from_str::<Chunk>(r#"{"offset":0,"bytes":"1","blake3":""}"#).is_err());
}
