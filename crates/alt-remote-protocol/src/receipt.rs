use crate::{Error, Limits, Publication, RequestKey, Result, hex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(rename_all = "snake_case")]
pub enum ErrorCode {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(rename_all = "snake_case")]
pub enum Retry {
    Never,
    /// Reuse the exact request identity and parameters; no new logical write.
    SameRequest,
    /// Query the original request before deciding whether another write exists.
    QueryOutcome,
    /// A rejected precondition requires a new intent and a fresh request ID.
    NewIntent,
}
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Failure {
    pub code: ErrorCode,
    pub message: String,
    pub request: Option<RequestKey>,
    pub retry: Retry,
}

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct ReceiptBody {
    pub publication: Publication,
    pub request_digest: String,
    #[json(with = "alt_json::with::decimal_u64")]
    pub transaction_sequence: u64,
    #[json(with = "alt_json::with::decimal_u64")]
    pub policy_version: u64,
    #[json(with = "alt_json::with::decimal_u64")]
    pub committed_at_unix_ms: u64,
}
impl ReceiptBody {
    pub fn validate(&self, limits: Limits) -> Result<()> {
        if self.publication.digest(limits)? != self.request_digest {
            return Err(Error::Invalid("receipt request digest"));
        }
        if self.transaction_sequence == 0
            || self.policy_version == 0
            || self.committed_at_unix_ms == 0
        {
            return Err(Error::Invalid("receipt transaction identity"));
        }
        Ok(())
    }
    /// Exact message to sign with the instance receipt key. Signing and key
    /// trust are adapters' responsibilities; syntactic validation is not proof.
    pub fn signing_bytes(&self, limits: Limits) -> Result<Vec<u8>> {
        self.validate(limits)?;
        let mut normalized = self.clone();
        normalized
            .publication
            .refs
            .sort_by(|a, b| a.name.cmp(&b.name));
        normalized
            .publication
            .record_roots
            .sort_by(|a, b| (a.kind.tag(), &a.id).cmp(&(b.kind.tag(), &b.id)));
        let mut bytes = b"alt-receipt-v1\0".to_vec();
        alt_json::to_writer(&mut bytes, &normalized).map_err(|e| Error::Json(e.to_string()))?;
        Ok(bytes)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub struct Receipt {
    pub body: ReceiptBody,
    /// BLAKE3 identity of the instance's Ed25519 public key.
    pub signing_key_id: String,
    /// Lowercase hexadecimal Ed25519 signature of body.signing_bytes().
    pub signature: String,
}
impl Receipt {
    pub fn validate(&self, limits: Limits) -> Result<()> {
        self.body.validate(limits)?;
        hex(&self.signing_key_id, 64, "receipt signing key")?;
        hex(&self.signature, 128, "receipt signature")
    }
}

/// Durable states. Unknown is deliberately absent: it describes a client's
/// inability to observe the authority, not a server decision to replay a write.
#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestState {
    Receiving {
        digest: String,
        #[json(with = "alt_json::with::decimal_u64")]
        expires_at_unix_ms: u64,
    },
    Ready {
        digest: String,
    },
    Committed {
        receipt: Box<Receipt>,
    },
    Rejected {
        digest: String,
        failure: Failure,
    },
    Expired {
        digest: String,
    },
}
impl RequestState {
    pub fn digest(&self) -> &str {
        match self {
            Self::Receiving { digest, .. }
            | Self::Ready { digest }
            | Self::Rejected { digest, .. }
            | Self::Expired { digest } => digest,
            Self::Committed { receipt } => &receipt.body.request_digest,
        }
    }
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Committed { .. } | Self::Rejected { .. } | Self::Expired { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(tag = "observation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestObservation {
    Recorded {
        key: RequestKey,
        result: Box<RequestState>,
    },
    /// The authority could not be reached or did not resolve the transaction.
    Unknown { key: RequestKey },
    /// Absence in this exact current epoch is not absence in an older backup.
    NotSeen { key: RequestKey },
}
