use alt_remote_protocol::{ContentKind, RecordKind, RecordRoot, Scope};
use alt_server_store::{Credential, Error, Page, PublishedRecord, Result, Store};
use alt_task_service::{RecordKind as TaskKind, TaskBundle, TaskLimits, TaskRecord};
use std::io::Read;

#[derive(alt_json::ToJson)]
pub struct RecordSummary {
    kind: RecordKind,
    id: String,
    title: String,
    published_by: String,
    published_at: String,
    task_id: Option<String>,
}
#[derive(alt_json::ToJson)]
pub struct RecordDetail {
    summary: RecordSummary,
    fields: Vec<Field>,
    related: Vec<Related>,
    attachments: Vec<Attachment>,
    publication: PublicationInfo,
    fields_truncated: bool,
}
#[derive(alt_json::ToJson)]
struct Field {
    label: &'static str,
    value: String,
}
#[derive(alt_json::ToJson)]
struct Related {
    kind: RecordKind,
    id: String,
    title: String,
}
#[derive(alt_json::ToJson)]
struct Attachment {
    name: String,
    bytes: String,
    download_url: Option<String>,
    omitted_reason: Option<String>,
}
#[derive(alt_json::ToJson)]
struct PublicationInfo {
    actor_id: String,
    request_id: String,
    digest: String,
    sequence: String,
}
fn public_kind(kind: TaskKind) -> Option<RecordKind> {
    Some(match kind {
        TaskKind::Task => RecordKind::Task,
        TaskKind::Candidate => RecordKind::Candidate,
        TaskKind::Evidence => RecordKind::Evidence,
        TaskKind::Review => RecordKind::Review,
        TaskKind::Integration => RecordKind::Integration,
        TaskKind::Delivery => RecordKind::Delivery,
        _ => return None,
    })
}
fn summary(root: &RecordRoot, record: &PublishedRecord) -> RecordSummary {
    let (title, task_id) = match &record.record {
        TaskRecord::Task(v) => (v.spec.purpose.clone(), Some(root.id.clone())),
        TaskRecord::Candidate(v) => (v.tree.clone(), Some(v.task.clone())),
        TaskRecord::Evidence(v) => (v.runner.clone(), None),
        TaskRecord::Review(v) => (v.reason.clone(), Some(v.task.clone())),
        TaskRecord::Integration(v) => (v.target.clone(), Some(v.task.clone())),
        TaskRecord::Delivery(v) => (v.destination.clone(), Some(v.task.clone())),
        _ => (root.id.clone(), None),
    };
    RecordSummary {
        kind: root.kind,
        id: root.id.clone(),
        title: title.chars().take(256).collect(),
        published_by: record.publication.publication.key.actor_id.clone(),
        published_at: record
            .publication
            .receipt
            .body
            .committed_at_unix_ms
            .to_string(),
        task_id,
    }
}
fn scope(store: &mut Store, credential: &Credential, repository: &str) -> Result<Scope> {
    Ok(store.repository_info(credential, repository)?.scope)
}
pub fn list(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    kind: RecordKind,
    after: &str,
    limit: usize,
) -> Result<Page<RecordSummary>> {
    if limit == 0 || limit >= 100 {
        return Err(Error::Limit("record page size"));
    }
    let scope = scope(store, credential, repository)?;
    let roots = store.published_records(
        credential,
        &scope,
        kind,
        if after.is_empty() { None } else { Some(after) },
        limit + 1,
    )?;
    let more = roots.len() > limit;
    let mut items = Vec::new();
    let mut remaining = 16 * 1024 * 1024;
    for root in roots.into_iter().take(limit) {
        let record = store.published_record(credential, &scope, &root, remaining)?;
        remaining = remaining
            .checked_sub(record.payload.bytes as usize)
            .ok_or(Error::Limit("record page work"))?;
        items.push(summary(&root, &record));
    }
    let next = if more {
        items.last().map(|v| v.id.clone())
    } else {
        None
    };
    Ok(Page { items, next })
}
pub fn detail(
    store: &mut Store,
    credential: &Credential,
    repository: &str,
    root: RecordRoot,
    limits: TaskLimits,
) -> Result<RecordDetail> {
    let scope = scope(store, credential, repository)?;
    let record = store.published_record(
        credential,
        &scope,
        &root,
        limits.structured_bytes.min(8 * 1024 * 1024),
    )?;
    let mut fields = Vec::new();
    let mut add = |label, value: String| fields.push(Field { label, value });
    match &record.record {
        TaskRecord::Task(v) => {
            add("purpose", v.spec.purpose.clone());
            add("authority", v.spec.authority.clone());
            add("allowedPaths", v.spec.allowed_paths.join("\n"));
            add("baseline", v.baseline.head.clone());
        }
        TaskRecord::Candidate(v) => {
            add("tree", v.tree.clone());
            add("source", v.source_selection.clone());
        }
        TaskRecord::Evidence(v) => {
            add("runner", v.runner.clone());
            add(
                "command",
                v.actual_context
                    .commands
                    .iter()
                    .map(|v| v.join(" "))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            add("outcome", alt_json::to_string(&v.termination)?);
            add("provenance", alt_json::to_string(&v.provenance)?);
        }
        TaskRecord::Review(v) => {
            add("historicalReview", v.reviewer.clone());
            add("reason", v.reason.clone());
            add("message", v.commit_message.clone());
        }
        TaskRecord::Integration(v) => {
            add("branch", v.target.clone());
            add("before", v.before.clone());
            add("commit", v.commit.clone());
        }
        TaskRecord::Delivery(v) => {
            add("destination", v.destination.clone());
        }
        _ => return Err(Error::Precondition("record is not a workbench root")),
    }
    let mut remaining = 2 * 1024 * 1024usize;
    let mut fields_truncated = false;
    for field in &mut fields {
        if field.value.len() > remaining {
            let mut boundary = remaining;
            while !field.value.is_char_boundary(boundary) {
                boundary -= 1;
            }
            field.value.truncate(boundary);
            fields_truncated = true;
        }
        remaining -= field.value.len();
    }
    let related = record
        .record
        .dependencies()
        .into_iter()
        .filter_map(|key| {
            public_kind(key.kind).map(|kind| Related {
                kind,
                title: key.id[..12].into(),
                id: key.id,
            })
        })
        .collect();
    let bundle_entry = record
        .publication
        .manifest
        .entries
        .iter()
        .find(|entry| entry.kind == ContentKind::TaskBundle)
        .ok_or(Error::Integrity("record task bundle"))?;
    if bundle_entry.bytes > limits.structured_bytes as u64 {
        return Err(Error::Limit("task bundle preview"));
    }
    let (bytes, file) = store.open_published_content(credential, &scope, &bundle_entry.blake3)?;
    if bytes != bundle_entry.bytes {
        return Err(Error::Integrity("bundle size"));
    }
    let mut raw = Vec::new();
    file.take(bytes + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 != bytes
        || alt_hash::blake3::hash(&raw).to_hex().as_str() != bundle_entry.blake3
    {
        return Err(Error::Integrity("bundle bytes"));
    }
    let bundle = TaskBundle::decode(&raw, limits)?;
    let mut attachments = vec![Attachment {
        name: "original-record.json".into(),
        bytes: record.payload.bytes.to_string(),
        download_url: Some(format!(
            "/web/repositories/{repository}/content/{}/download",
            record.payload.blake3
        )),
        omitted_reason: None,
    }];
    for artifact in record.record.artifacts() {
        let selected = record
            .publication
            .manifest
            .entries
            .iter()
            .any(|entry| entry.blake3 == artifact.blake3 && entry.bytes == artifact.bytes);
        let reason = bundle
            .omissions
            .iter()
            .find(|item| item.owner.id == root.id && item.artifact == artifact)
            .map(|item| item.reason.clone());
        if !selected && reason.is_none() {
            return Err(Error::Integrity("record attachment disposition"));
        }
        attachments.push(Attachment {
            name: artifact_name(&record.record, &artifact.blake3),
            bytes: artifact.bytes.to_string(),
            download_url: if selected {
                Some(format!(
                    "/web/repositories/{repository}/content/{}/download",
                    artifact.blake3
                ))
            } else {
                None
            },
            omitted_reason: reason,
        });
    }
    let receipt = &record.publication.receipt;
    Ok(RecordDetail {
        summary: summary(&root, &record),
        fields,
        fields_truncated,
        related,
        attachments,
        publication: PublicationInfo {
            actor_id: record.publication.publication.key.actor_id.clone(),
            request_id: record.publication.publication.key.request_id.clone(),
            digest: receipt.body.request_digest.clone(),
            sequence: receipt.body.transaction_sequence.to_string(),
        },
    })
}
fn artifact_name(record: &TaskRecord, digest: &str) -> String {
    match record {
        TaskRecord::Evidence(e) => {
            if e.stdout.as_ref().is_some_and(|v| v.blake3 == digest) {
                return "stdout".into();
            }
            if e.stderr.as_ref().is_some_and(|v| v.blake3 == digest) {
                return "stderr".into();
            }
            for (name, artifact) in e.artifacts.iter().chain(e.binaries.iter()) {
                if artifact.blake3 == digest {
                    return name.clone();
                }
            }
            if e.original_report
                .as_ref()
                .is_some_and(|v| v.blake3 == digest)
            {
                return "original-report".into();
            }
        }
        TaskRecord::Review(_) => return "original-changes.json".into(),
        TaskRecord::Delivery(_) => return "original-delivery-report".into(),
        _ => {}
    }
    digest.into()
}

#[cfg(test)]
mod json_tests {
    use alt_json_testkit::fixtures::{assert_stored, written_line};
    use alt_json_testkit::rng::Rng;
    use alt_json_testkit::support::Arbitrary;

    use super::*;

    fn any<T: Arbitrary>(rng: &mut Rng) -> T {
        T::arbitrary(rng)
    }

    fn kind(rng: &mut Rng) -> RecordKind {
        *rng.pick(&[
            RecordKind::Task,
            RecordKind::Candidate,
            RecordKind::Evidence,
            RecordKind::Review,
            RecordKind::Integration,
            RecordKind::Delivery,
        ])
    }

    fn summary(rng: &mut Rng) -> RecordSummary {
        RecordSummary {
            kind: kind(rng),
            id: any(rng),
            title: any(rng),
            published_by: any(rng),
            published_at: any(rng),
            task_id: any(rng),
        }
    }

    #[test]
    fn responses_are_written_as_recorded() {
        let mut rng = Rng::new(0xd600);
        let rng = &mut rng;
        let mut lines = String::new();
        for _ in 0..40 {
            lines += &written_line(&Page {
                items: (0..rng.below(3)).map(|_| summary(rng)).collect(),
                next: any(rng),
            });
            lines += &written_line(&RecordDetail {
                summary: summary(rng),
                fields: (0..rng.below(3))
                    .map(|_| Field {
                        label: "Title",
                        value: any(rng),
                    })
                    .collect(),
                related: (0..rng.below(3))
                    .map(|_| Related {
                        kind: kind(rng),
                        id: any(rng),
                        title: any(rng),
                    })
                    .collect(),
                attachments: (0..rng.below(3))
                    .map(|_| Attachment {
                        name: any(rng),
                        bytes: any(rng),
                        download_url: any(rng),
                        omitted_reason: any(rng),
                    })
                    .collect(),
                publication: PublicationInfo {
                    actor_id: any(rng),
                    request_id: any(rng),
                    digest: any(rng),
                    sequence: any(rng),
                },
                fields_truncated: any(rng),
            });
        }
        assert_stored(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/json/records-responses.txt"
            ),
            &lines,
        );
    }
}
