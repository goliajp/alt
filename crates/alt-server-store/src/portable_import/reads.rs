//! What is asked about an import while it is received. These read the repository
//! and the import as one moment has them and lock neither: a push to the
//! repository, or the upload the import goes on with, is not waited for.
use super::{PortableImportState, active, authorize_write, authorized, state};
use crate::Jsonb;
use crate::database::Transaction;
use crate::reading::twice;
use crate::store::{instance, now};
use crate::{Credential, Error, Result, Store};
use alt_pg::Row;
use alt_portable::{Blob, Envelope};
use alt_remote_protocol::RequestKey;

fn row(tx: &mut Transaction<'_>, key: &RequestKey) -> Result<Option<Row>> {
    Ok(tx.query_opt(
        "SELECT * FROM alt_portable_imports WHERE repository=$1 AND import_id=$2",
        &[&key.scope.repository_instance_id, &key.request_id],
    )?)
}

impl Store {
    pub fn query_portable_import(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<PortableImportState> {
        twice(|| self.query_portable_import_once(credential, key))
    }
    fn query_portable_import_once(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
    ) -> Result<PortableImportState> {
        key.validate()?;
        let mut tx = crate::reading::transaction(&mut self.client, &self.spec)?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let repository = &key.scope.repository_instance_id;
        let current = crate::reading::snapshot(&mut tx, &self.spec, repository)?;
        let (current, _) = authorized(&mut tx, &self.spec, credential, key, current)?;
        let result = match row(&mut tx, key)? {
            Some(row) => state(&mut tx, &row, key, &current.scope)?,
            None if current.scope == key.scope => PortableImportState::NotSeen { key: key.clone() },
            None => PortableImportState::Unknown { key: key.clone() },
        };
        tx.commit()?;
        Ok(result)
    }
    /// Validate an immutable upload selection before allocating request-body
    /// storage. Registration repeats these checks after durable file creation.
    pub fn selected_portable_upload(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<Blob> {
        twice(|| self.selected_portable_upload_once(credential, key, digest))
    }
    fn selected_portable_upload_once(
        &mut self,
        credential: &Credential,
        key: &RequestKey,
        digest: &str,
    ) -> Result<Blob> {
        key.validate()?;
        let mut tx = crate::reading::transaction(&mut self.client, &self.spec)?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let repository = &key.scope.repository_instance_id;
        let current = crate::reading::snapshot(&mut tx, &self.spec, repository)?;
        let (current, auth) = authorized(&mut tx, &self.spec, credential, key, current)?;
        let import = Some(key.request_id.as_str());
        authorize_write(&mut tx, &self.spec, key, &current, &auth, import)?;
        let row = row(&mut tx, key)?.ok_or(Error::Precondition("portable import not found"))?;
        active(&row, key, now(&mut tx)?)?;
        let envelope: Envelope = row.get::<_, Jsonb>("envelope").read()?;
        let found = (envelope.catalog.blobs.into_iter()).find(|blob| blob.blake3 == digest);
        let blob = found.ok_or(Error::Unauthorized)?;
        tx.commit()?;
        Ok(blob)
    }
}
