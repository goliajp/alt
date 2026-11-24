//! The reads that are begun again where they were overtaken.

use super::twice;
use crate::transport::Led;
use crate::{Credential, Result, Store};
use alt_remote_protocol::{ContentEntry, RecordRoot, RequestKey, Scope};
use golia_change_link::HistoricalEvent;
use std::collections::BTreeMap;

impl Store {
    pub fn published_records(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        kind: alt_remote_protocol::RecordKind,
        after: Option<&str>,
        count: usize,
    ) -> Result<Vec<RecordRoot>> {
        twice(|| self.published_records_once(credential, scope, kind, after, count))
    }

    pub fn repository_info(
        &mut self,
        credential: &Credential,
        repository: &str,
    ) -> Result<alt_remote_protocol::RepositoryInfo> {
        twice(|| self.repository_info_once(credential, repository))
    }

    pub fn query(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<alt_remote_protocol::RequestObservation> {
        twice(|| self.query_once(credential, key))
    }

    pub fn change_channels(
        &mut self,
        credential: &Credential,
        scope: &Scope,
    ) -> Result<Vec<crate::ChangeChannelStatus>> {
        twice(|| self.change_channels_once(credential, scope))
    }

    pub fn change_events(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        after: Option<&str>,
    ) -> Result<crate::ChangeEventPage> {
        twice(|| self.change_events_once(credential, scope, after))
    }

    pub fn change_request_event(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<Option<HistoricalEvent>> {
        twice(|| self.change_request_event_once(credential, key))
    }

    /// Original signed fact under current repository read permission. A
    /// migrated or disabled pairing remains history, never live authorization.
    pub fn change_event(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
    ) -> Result<HistoricalEvent> {
        twice(|| self.change_event_once(credential, scope, digest))
    }

    pub fn published_record(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        root: &RecordRoot,
        max_bytes: usize,
    ) -> Result<crate::PublishedRecord> {
        twice(|| self.published_record_once(credential, scope, root, max_bytes))
    }

    /// Select published task bundles by any selected immutable record. Original manifest,
    /// publication and receipt are retained; a receiving client never invents
    /// a local request completion for the originating workspace.
    pub fn bundles_for_record(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        root: &RecordRoot,
        after_digest: Option<&str>,
        count: usize,
    ) -> Result<Vec<crate::PublishedBundle>> {
        twice(|| self.bundles_for_record_once(credential, scope, root, after_digest, count))
    }

    /// A published payload by its digest. A Git object of a repository that
    /// holds what no ref leads to is given where a ref the caller reads leads
    /// to it, or the manifest of a published publication of records names it.
    pub fn open_published_content(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
    ) -> Result<(u64, crate::StoredReader)> {
        self.open_published_content_while(credential, scope, digest, None)
    }

    /// [`Self::open_published_content`], with what the refs lead to looked
    /// for no longer once `gone` is set.
    pub fn open_published_content_while(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
        gone: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(u64, crate::StoredReader)> {
        let led = (Led::RefsOrRecords, gone);
        twice(|| self.open_published_content_once(credential, scope, digest, led))
    }

    /// [`Self::open_published_content`] for a caller that is shown the
    /// repository by its refs alone: a Git object is given where a ref leads
    /// to it. What the refs lead to is looked for no longer once `gone` is set.
    pub fn open_reached_content(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        digest: &str,
        gone: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(u64, crate::StoredReader)> {
        let led = (Led::Refs, gone);
        twice(|| self.open_published_content_once(credential, scope, digest, led))
    }

    /// Early admission for Git receive-pack before reading a potentially large
    /// body. Exact per-ref/path/force checks still occur at publication.
    pub fn check_git_receive_access(
        &mut self,
        credential: &Credential,
        repository: &str,
    ) -> Result<()> {
        twice(|| self.check_git_receive_access_once(credential, repository))
    }

    /// Query never captures a new snapshot. An obsolete scope cannot resume an
    /// export from before a restore, even if the original ID remains on disk.
    pub fn query_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: alt_portable::PackageLimits,
    ) -> Result<crate::PortableExportState> {
        twice(|| self.query_portable_export_once(credential, scope, id, limits))
    }

    /// Read-only archive inspection consumes bounded server resources under
    /// current administrator and repository-read authority, without a grant.
    pub fn check_portable_inspection(
        &mut self,
        credential: &Credential,
        scope: &Scope,
    ) -> Result<()> {
        twice(|| self.check_portable_inspection_once(credential, scope))
    }

    pub(crate) fn upload_chunks(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<(BTreeMap<String, u64>, Vec<String>)> {
        twice(|| self.upload_chunks_once(credential, key))
    }

    pub fn selected_chunk(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<u64> {
        twice(|| self.selected_chunk_once(credential, key, digest))
    }

    /// the selected entries, and whether the request already has their content
    pub(crate) fn selected_upload_state(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<(Vec<ContentEntry>, bool)> {
        twice(|| self.selected_upload_state_once(credential, key, digest))
    }

    /// The snapshot is still this actor's to read: same repository epoch, current
    /// grants, and neither the snapshot nor the credential has run out.
    pub(crate) fn check_read_snapshot(
        &mut self,
        credential: &Credential,
        read: &crate::ReadSnapshot,
    ) -> Result<()> {
        twice(|| self.check_read_snapshot_once(credential, read))
    }
}
