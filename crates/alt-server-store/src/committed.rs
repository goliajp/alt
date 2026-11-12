//! How a committed publication is kept. The row of its request holds the publication
//! once. The stored state of the request and the outbox event hold the receipt
//! without it, and every read puts it back from that row. The audit record is a fact
//! of its own and holds the whole receipt.

use crate::jsonb::JsonText;
use crate::{Error, Jsonb, Result, Store, database::Transaction, store::integer};
use alt_remote_protocol::{Receipt, RequestKey};

/// the state of request row `q` as the protocol has it
macro_rules! state_sql {
    () => {
        "CASE WHEN q.state='committed' THEN jsonb_set(q.result,'{receipt,body,publication}',q.publication) ELSE q.result END"
    };
}
/// the receipt of committed request row `q`
macro_rules! receipt_sql {
    () => {
        "jsonb_set(q.result->'receipt','{body,publication}',q.publication)"
    };
}
/// The event outbox row `o` stands for, which is what its audit record holds. It is
/// null when the request the row names is not there.
macro_rules! outbox_event_sql {
    () => {
        "CASE WHEN o.event ? 'request' THEN (SELECT jsonb_build_object('kind',o.event->'kind','receipt',jsonb_set(o.event->'receipt','{body,publication}',q.publication)) FROM alt_requests q WHERE q.repository=o.repository AND q.epoch=(o.event#>>'{request,scope,recovery_epoch}')::bigint AND q.actor_id=o.event#>>'{request,actor_id}' AND q.request_id=o.event#>>'{request,request_id}') ELSE o.event END"
    };
}
pub(crate) use {outbox_event_sql, receipt_sql, state_sql};

/// The row of a request, with its state as the protocol has it in `outcome`: `result`
/// alone is not that state once the request is committed.
pub(crate) fn request_row(
    tx: &mut Transaction<'_>,
    key: &RequestKey,
) -> Result<Option<alt_pg::Row>> {
    Ok(tx.query_opt(concat!("SELECT q.*,", state_sql!(), " AS outcome FROM alt_requests q WHERE q.repository=$1 AND q.epoch=$2 AND q.actor_id=$3 AND q.request_id=$4 FOR UPDATE"),&[&key.scope.repository_instance_id,&integer(key.scope.recovery_epoch)?,&key.actor_id,&key.request_id])?)
}

/// a receipt without its publication, field for field
#[derive(alt_json::ToJson)]
struct Stored {
    body: StoredBody,
    signing_key_id: String,
    signature: String,
}
#[derive(alt_json::ToJson)]
struct StoredBody {
    request_digest: String,
    #[json(with = "alt_json::with::decimal_u64")]
    transaction_sequence: u64,
    #[json(with = "alt_json::with::decimal_u64")]
    policy_version: u64,
    #[json(with = "alt_json::with::decimal_u64")]
    committed_at_unix_ms: u64,
}

/// what one committed publication writes besides its publication
pub(crate) struct Documents {
    /// the audit record: the event with the whole receipt
    pub audit: Vec<u8>,
    /// the outbox event: the key of the request and the receipt without the publication
    pub outbox: Vec<u8>,
    /// the stored state of the request: committed, with the same shortened receipt
    pub result: Vec<u8>,
}

pub(crate) fn committed_documents(receipt: &Receipt) -> Result<Documents> {
    let whole = alt_json::to_vec(receipt)?;
    let stored = alt_json::to_vec(&Stored {
        body: StoredBody {
            request_digest: receipt.body.request_digest.clone(),
            transaction_sequence: receipt.body.transaction_sequence,
            policy_version: receipt.body.policy_version,
            committed_at_unix_ms: receipt.body.committed_at_unix_ms,
        },
        signing_key_id: receipt.signing_key_id.clone(),
        signature: receipt.signature.clone(),
    })?;
    let key = alt_json::to_vec(&receipt.body.publication.key)?;
    Ok(Documents {
        audit: [
            br#"{"kind":"publication_committed","receipt":"#.as_slice(),
            &whole,
            b"}",
        ]
        .concat(),
        outbox: [
            br#"{"kind":"publication_committed","request":"#.as_slice(),
            &key,
            br#","receipt":"#,
            &stored,
            b"}",
        ]
        .concat(),
        result: [
            br#"{"state":"committed","receipt":"#.as_slice(),
            &stored,
            b"}",
        ]
        .concat(),
    })
}

/// the audit record of a committed publication and its outbox event, in one statement
pub(crate) fn record_committed(
    tx: &mut Transaction<'_>,
    repository: &str,
    sequence: u64,
    actor: &str,
    documents: &Documents,
) -> Result<()> {
    tx.execute(
        "WITH audit AS (INSERT INTO alt_audit(repository,sequence,actor_id,event) VALUES($1,$2,$3,$4::jsonb)) INSERT INTO alt_outbox(repository,sequence,event) VALUES($1,$2,$5::jsonb)",
        &[
            &repository,
            &integer(sequence)?,
            &actor,
            &JsonText(&documents.audit),
            &JsonText(&documents.outbox),
        ],
    )?;
    Ok(())
}

impl Store {
    /// The whole event of an outbox row, for whoever delivers it: the row of a
    /// committed publication names its request, and the publication is read from
    /// there. `None` when the repository has no outbox row at that sequence.
    pub fn outbox_event(&mut self, repository: &str, sequence: u64) -> Result<Option<Jsonb>> {
        let mut tx = self.client.transaction()?;
        let row = tx.query_opt(
            concat!(
                "SELECT ",
                outbox_event_sql!(),
                " FROM alt_outbox o WHERE o.repository=$1 AND o.sequence=$2"
            ),
            &[&repository, &integer(sequence)?],
        )?;
        tx.commit()?;
        row.map(|row| {
            row.get::<_, Option<Jsonb>>(0)
                .ok_or(Error::Integrity("outbox event names no stored request"))
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alt_remote_protocol::{
        ActorKind, Attribution, ObjectFormat, Publication, ReceiptBody, RefUpdate, RequestKey,
        Scope, VERSION,
    };

    fn receipt() -> Receipt {
        Receipt {
            body: ReceiptBody {
                publication: Publication {
                    protocol_version: VERSION,
                    key: RequestKey {
                        scope: Scope {
                            installation_id: "installation".into(),
                            repository_instance_id: "repository".into(),
                            recovery_epoch: 1,
                        },
                        actor_id: "owner".into(),
                        request_id: "git-00".into(),
                    },
                    attribution: Attribution {
                        kind: ActorKind::Human,
                        on_behalf_of: None,
                        delegation_id: None,
                        agent_session: None,
                    },
                    object_format: ObjectFormat::Sha1,
                    refs: vec![RefUpdate {
                        name: "refs/heads/\"quoted\"".into(),
                        expected: None,
                        new: Some("1".repeat(40)),
                        force: false,
                    }],
                    record_roots: vec![],
                    content_manifest: "0".repeat(64),
                },
                request_digest: "2".repeat(64),
                transaction_sequence: 7,
                policy_version: 1,
                committed_at_unix_ms: 1_700_000_000_000,
            },
            signing_key_id: "3".repeat(64),
            signature: "4".repeat(128),
        }
    }

    #[test]
    fn the_audit_record_holds_the_whole_receipt() {
        let receipt = receipt();
        let documents = committed_documents(&receipt).unwrap();
        assert_eq!(
            alt_json::parse(&documents.audit).unwrap(),
            alt_json::json!({"kind":"publication_committed","receipt":receipt})
        );
    }

    #[test]
    fn the_stored_state_and_the_outbox_event_lack_only_the_publication() {
        let receipt = receipt();
        let documents = committed_documents(&receipt).unwrap();
        let publication = alt_json::to_value(&receipt.body.publication).unwrap();
        let whole = alt_json::to_value(&receipt).unwrap();

        let mut result = alt_json::parse(&documents.result).unwrap();
        assert_eq!(result["state"], "committed");
        assert!(result["receipt"]["body"].get("publication").is_none());
        result["receipt"]["body"]["publication"] = publication.clone();
        assert_eq!(result["receipt"], whole);
        let state: alt_remote_protocol::RequestState = alt_json::from_value(result).unwrap();
        assert_eq!(
            state,
            alt_remote_protocol::RequestState::Committed {
                receipt: Box::new(receipt.clone())
            }
        );

        let mut outbox = alt_json::parse(&documents.outbox).unwrap();
        assert_eq!(outbox["kind"], "publication_committed");
        assert_eq!(
            alt_json::from_value::<RequestKey>(outbox["request"].clone()).unwrap(),
            receipt.body.publication.key
        );
        outbox["receipt"]["body"]["publication"] = publication;
        assert_eq!(outbox["receipt"], whole);
    }
}
