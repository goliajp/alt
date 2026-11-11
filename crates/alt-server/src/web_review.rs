//! Browser projection of the same verified review used by native approval.
use crate::NativeOptions;
use alt_json::{Value, json};
use alt_remote_protocol::{RecordKind, RequestKey};
use alt_server_store::{Credential, Error, Result, Store};
use alt_task_service::{ArtifactSource, RecordKey, TaskRecord, VerifiedPublication};
use serde::Deserialize;
use std::io::Read;

#[derive(Deserialize, alt_json::FromJson)]
struct Changes {
    changes: Vec<Change>,
}
#[derive(Deserialize, alt_json::FromJson)]
struct Change {
    path: String,
    before: Option<Version>,
    after: Option<Version>,
}
#[derive(Deserialize, alt_json::FromJson)]
struct Version {
    oid: String,
    mode: u32,
    utf8: Option<String>,
}
pub fn inspect(
    store: &mut Store,
    credential: &Credential,
    options: &NativeOptions,
    key: &RequestKey,
) -> Result<Value> {
    let limits = store.limits();
    let work = store.acquire_review(
        credential,
        key,
        options.worker_lease_ms,
        limits.graph_objects,
    )?;
    let result = (|| {
        let verified = VerifiedPublication::verify(
            work.publication(),
            work.manifest(),
            work.source(),
            work.artifacts(),
            limits,
            options.task_limits,
        )?;
        let tasks = verified
            .tasks
            .as_ref()
            .ok_or(Error::Precondition("task review required"))?;
        let graph = tasks.prepared();
        let mut integrations = std::collections::BTreeSet::new();
        for root in &work.publication().record_roots {
            match root.kind {
                RecordKind::Integration => {
                    integrations.insert(root.id.clone());
                }
                RecordKind::Delivery => {
                    let TaskRecord::Delivery(delivery) = graph.record(&RecordKey {
                        kind: alt_task_service::RecordKind::Delivery,
                        id: root.id.clone(),
                    })?
                    else {
                        return Err(Error::Integrity("delivery record"));
                    };
                    integrations.insert(delivery.integration.clone());
                }
                _ => {}
            }
        }
        if integrations.len() != 1 {
            return Err(Error::Precondition(
                "select one integration for browser review",
            ));
        }
        let integration_id = integrations.into_iter().next().unwrap();
        let TaskRecord::Integration(integration) = graph.record(&RecordKey {
            kind: alt_task_service::RecordKind::Integration,
            id: integration_id.clone(),
        })?
        else {
            return Err(Error::Integrity("integration record"));
        };
        let TaskRecord::Review(review) = graph.record(&RecordKey {
            kind: alt_task_service::RecordKind::Review,
            id: integration.review.clone(),
        })?
        else {
            return Err(Error::Integrity("review record"));
        };
        let TaskRecord::Evidence(evidence) = graph.record(&RecordKey {
            kind: alt_task_service::RecordKind::Evidence,
            id: review.evidence.clone(),
        })?
        else {
            return Err(Error::Integrity("evidence record"));
        };
        let current = store.inspect_current_integration(
            credential,
            &work,
            &verified.graph,
            tasks,
            &integration_id,
        )?;
        let inspected = current.selection.approval_context(limits)?;
        let mut bytes = Vec::new();
        if review.changes.bytes > options.task_limits.structured_bytes as u64 {
            return Err(Error::Limit("review bytes"));
        }
        work.artifacts()
            .open(&review.changes)?
            .take(review.changes.bytes + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != review.changes.bytes
            || alt_hash::blake3::hash(&bytes).to_hex().as_str() != review.changes.blake3
        {
            return Err(Error::Integrity("review original bytes"));
        }
        let original: Changes = alt_json::from_slice(&bytes)?;
        let mut remaining = 2 * 1024 * 1024usize;
        let mut projected = Vec::new();
        let mut truncated = false;
        for change in original.changes {
            if projected.len() >= 2000 {
                truncated = true;
                break;
            }
            let text_size = change
                .before
                .as_ref()
                .and_then(|v| v.utf8.as_ref())
                .map_or(0, String::len)
                + change
                    .after
                    .as_ref()
                    .and_then(|v| v.utf8.as_ref())
                    .map_or(0, String::len);
            let binary = change.before.as_ref().is_some_and(|v| v.utf8.is_none())
                || change.after.as_ref().is_some_and(|v| v.utf8.is_none());
            let patch = if !binary && text_size.saturating_mul(2).saturating_add(128) <= remaining {
                let mut text =
                    String::from("--- before (complete file)\n+++ after (complete file)\n");
                if let Some(before) = change.before.as_ref().and_then(|v| v.utf8.as_deref()) {
                    for line in before.split('\n') {
                        text.push('-');
                        text.push_str(line);
                        text.push('\n');
                    }
                }
                if let Some(after) = change.after.as_ref().and_then(|v| v.utf8.as_deref()) {
                    for line in after.split('\n') {
                        text.push('+');
                        text.push_str(line);
                        text.push('\n');
                    }
                }
                remaining = remaining.saturating_sub(text.len());
                Some(text)
            } else {
                if !binary {
                    truncated = true;
                }
                None
            };
            projected.push(json!({"path":change.path,"kind":if change.before.is_none(){"added"}else if change.after.is_none(){"deleted"}else{"modified"},"before":change.before.as_ref().map(|v|&v.oid),"after":change.after.as_ref().map(|v|&v.oid),"before_mode":change.before.as_ref().map(|v|v.mode),"after_mode":change.after.as_ref().map(|v|v.mode),"patch":patch}));
        }
        work.check_read_integrity()?;
        let original_url = format!(
            "/web/repositories/{}/epochs/{}/requests/{}/{}/content/{}/download",
            key.scope.repository_instance_id,
            key.scope.recovery_epoch,
            key.actor_id,
            key.request_id,
            review.changes.blake3
        );
        Ok(
            json!({"key":key,"inspected":inspected,"integration_id":integration_id,"candidate_id":review.candidate,"historical_reviewer":review.reviewer,"changes":projected,"changes_truncated":truncated,"original_changes_url":original_url,"evidence":[{"id":review.evidence,"command":evidence.actual_context.commands.iter().map(|v|v.join(" ")).collect::<Vec<_>>().join("\n"),"outcome":alt_json::to_string(&evidence.termination)?,"source":current.evidence.source_proof,"accepted":current.evidence.eligible,"reason":current.evidence.reasons.join(", ")}],"approved":current.approved,"eligible":current.eligible,"reason":current.reason}),
        )
    })();
    let released = store.release_publication(credential, &work);
    match result {
        Ok(value) => {
            released?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod json_tests {
    use alt_json::{Value, json};
    use alt_json_testkit::drive::exercise_reader;
    use alt_json_testkit::fixtures::{check_recorded, record_reader, verify_reader};
    use alt_json_testkit::rng::Rng;
    use alt_json_testkit::support::Arbitrary;

    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");

    fn version(rng: &mut Rng) -> Value {
        if rng.chance(3) {
            return Value::Null;
        }
        json!({
            "oid": String::arbitrary(rng),
            "mode": u32::arbitrary(rng),
            "utf8": Option::<String>::arbitrary(rng),
        })
    }

    fn changes(rng: &mut Rng) -> Value {
        let changes: Vec<Value> = (0..rng.below(3))
            .map(|_| json!({"path": String::arbitrary(rng), "before": version(rng), "after": version(rng)}))
            .collect();
        json!({"changes": changes})
    }

    fn view(value: &Changes) -> String {
        let version = |v: &Option<Version>| {
            v.as_ref()
                .map(|v| format!("{:?}", (&v.oid, v.mode, &v.utf8)))
        };
        let changes: Vec<_> = value
            .changes
            .iter()
            .map(|c| (&c.path, version(&c.before), version(&c.after)))
            .collect();
        format!("{changes:?}")
    }

    #[test]
    fn same_as_serde() {
        exercise_reader::<Changes>(0xd500, 300, changes, view).report("review changes");
    }

    #[test]
    fn recorded_verdicts() {
        check_recorded(
            &format!("{DIR}/web_review-Changes.txt"),
            &record_reader::<Changes>(0xd501, 15, changes),
        );
        verify_reader::<Changes>(DIR, "web_review-Changes");
    }
}
