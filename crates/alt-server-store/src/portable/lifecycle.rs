//! Opening, freezing, reopening, retiring and querying a captured export.
use super::*;

impl Store {
    /// Resume an existing immutable export. This read never silently captures
    /// a newer snapshot, nor does an old ID bypass current account authority.
    pub fn open_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: alt_portable::PackageLimits,
    ) -> Result<PortableExport> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let row = tx.query_opt("SELECT envelope,documents FROM alt_portable_exports WHERE repository=$1 AND export_id=$2 AND NOT retired AND verified_at IS NOT NULL", &[&scope.repository_instance_id,&id])?.ok_or(Error::Precondition("active portable export not found"))?;
        let envelope: Envelope = serde_json::from_value(row.get(0))?;
        envelope.verify(scope, self.signing_key.verifying_key().as_bytes(), limits)?;
        let locations = export_locations(&mut tx, scope, &envelope.catalog.blobs)?;
        let documents = serde_json::from_value(row.get(1))?;
        let ids = Self::live_segment_ids(&mut tx, &scope.repository_instance_id)?;
        tx.commit()?;
        let (_, segments) = self.export_segments(&scope.repository_instance_id, ids)?;
        Ok(PortableExport {
            locations,
            envelope,
            documents,
            bytes: self.bytes.clone(),
            segments,
        })
    }

    /// The source remains present. Only an unchanged snapshot can become the
    /// cutover watermark, atomically blocking both new and in-flight writes.
    pub fn freeze_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: alt_portable::PackageLimits,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let repository = &scope.repository_instance_id;
        let row = tx.query_opt("SELECT envelope FROM alt_portable_exports WHERE repository=$1 AND export_id=$2 AND NOT retired AND verified_at IS NOT NULL", &[repository,&id])?.ok_or(Error::Precondition("active portable export not found"))?;
        let envelope: Envelope = serde_json::from_value(row.get(0))?;
        envelope.verify(scope, self.signing_key.verifying_key().as_bytes(), limits)?;
        let catalog = &envelope.catalog;
        let row = tx.query_one("SELECT transaction_sequence,default_branch,portable_freeze FROM alt_repositories WHERE id=$1", &[repository])?;
        let previous: Option<String> = row.get(2);
        if previous.as_deref().is_some_and(|old| old != id) {
            return Err(Error::Precondition(
                "another portable export freezes this repository",
            ));
        }
        if catalog.source != *scope
            || catalog.snapshot_sequence != unsigned(row.get(0))?
            || catalog.default_branch != row.get::<_, String>(1)
            || serde_json::to_value(&catalog.protections)?
                != serde_json::to_value(&current.policies)?
            || catalog
                .refs
                .iter()
                .map(|r| (r.name.clone(), r.oid.clone()))
                .collect::<BTreeMap<_, _>>()
                != current
                    .refs
                    .iter()
                    .map(|(name, oid)| (name.clone(), oid.to_string()))
                    .collect()
        {
            return Err(Error::Precondition(
                "source changed; capture a new portable export",
            ));
        }
        if previous.is_none() {
            tx.execute(
                "UPDATE alt_repositories SET portable_freeze=$2 WHERE id=$1",
                &[repository, &id],
            )?;
            audit(&mut tx, &auth.actor_id, "portable.export.frozen", scope, id)?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }

    /// Explicitly reopen this source after inspecting the destination. This
    /// never rolls back a destination or changes paid/service write authority.
    /// Current owners may also release a freeze preserved by disaster recovery.
    pub fn release_portable_freeze(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let repository = &scope.repository_instance_id;
        let previous: Option<String> = tx
            .query_one(
                "SELECT portable_freeze FROM alt_repositories WHERE id=$1",
                &[repository],
            )?
            .get(0);
        if previous.as_deref().is_some_and(|old| old != id) {
            return Err(Error::Precondition("source freeze identity changed"));
        }
        if previous.is_some() {
            tx.execute(
                "UPDATE alt_repositories SET portable_freeze=NULL WHERE id=$1",
                &[repository],
            )?;
            audit(
                &mut tx,
                &auth.actor_id,
                "portable.export.reopened",
                scope,
                id,
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }

    /// Retire only this snapshot's pins. Source refs, task history and the
    /// durable export identity remain, while another export can reuse its slot.
    pub fn retire_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if &current.scope != scope {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let repository = &scope.repository_instance_id;
        let frozen: Option<String> = tx
            .query_one(
                "SELECT portable_freeze FROM alt_repositories WHERE id=$1",
                &[repository],
            )?
            .get(0);
        if frozen.as_deref() == Some(id) {
            return Err(Error::Precondition(
                "explicitly release the source freeze before retiring its snapshot",
            ));
        }
        let row = tx
            .query_opt(
                "SELECT retired FROM alt_portable_exports WHERE repository=$1 AND export_id=$2",
                &[repository, &id],
            )?
            .ok_or(Error::Precondition("portable export not found"))?;
        if !row.get::<_, bool>(0) {
            tx.execute("UPDATE alt_portable_exports SET retired=true,documents='{}' WHERE repository=$1 AND export_id=$2", &[repository,&id])?;
            tx.execute(
                "DELETE FROM alt_pins WHERE repository=$1 AND pin_id=$2",
                &[repository, &pin_id(id)],
            )?;
            audit(
                &mut tx,
                &auth.actor_id,
                "portable.export.retired",
                scope,
                id,
            )?;
        }
        tx.commit().map_err(|_| Error::OutcomeUnknown)
    }
}

impl Store {
    /// Query never captures a new snapshot. An obsolete scope cannot resume an
    /// export from before a restore, even if the original ID remains on disk.
    pub fn query_portable_export(
        &mut self,
        credential: &Credential,
        scope: &Scope,
        id: &str,
        limits: alt_portable::PackageLimits,
    ) -> Result<PortableExportState> {
        use PortableExportState as ExportState;
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if scope.installation_id != self.spec.installation_id {
            return Err(Error::Unauthorized);
        }
        export_key(scope, &auth.actor_id, id)?;
        let unknown = || ExportState::Unknown {
            scope: scope.clone(),
            export_id: id.into(),
        };
        let result = if current.scope != *scope {
            unknown()
        } else if let Some(row) = tx.query_opt(
            "SELECT envelope,retired,verified_at FROM alt_portable_exports WHERE repository=$1 AND export_id=$2",
            &[&scope.repository_instance_id, &id],
        )? {
            let envelope: Envelope = serde_json::from_value(row.get(0))?;
            if envelope.catalog.source != *scope {
                unknown()
            } else {
                let digest = envelope.verify(scope, self.signing_key.verifying_key().as_bytes(), limits)?;
                if row.get::<_,bool>(1) {
                    ExportState::Retired { scope: scope.clone(), export_id: id.into(), catalog_digest: digest }
                } else if row.get::<_,Option<i64>>(2).is_none() {
                    ExportState::Preparing { scope: scope.clone(), export_id:id.into(), catalog_digest:digest, snapshot_sequence:envelope.catalog.snapshot_sequence }
                } else {
                    let frozen: Option<String> = tx.query_one("SELECT portable_freeze FROM alt_repositories WHERE id=$1", &[&scope.repository_instance_id])?.get(0);
                    ExportState::Available { envelope: Box::new(envelope), frozen: frozen.as_deref() == Some(id) }
                }
            }
        } else {
            ExportState::NotSeen { scope: scope.clone(), export_id: id.into() }
        };
        tx.commit()?;
        Ok(result)
    }
}

impl Store {
    /// Read-only archive inspection consumes bounded server resources under
    /// current administrator and repository-read authority, without a grant.
    pub fn check_portable_inspection(
        &mut self,
        credential: &Credential,
        scope: &Scope,
    ) -> Result<()> {
        let mut tx = self.client.transaction()?;
        instance(&mut tx, &self.spec, &self.signing_key)?;
        let current = snapshot(&mut tx, &self.spec, &scope.repository_instance_id)?;
        require_admin(&mut tx, &self.spec, credential)?;
        let auth = authorize(&mut tx, &self.spec, credential, &current)?;
        if current.scope != *scope || auth.attribution.kind != alt_remote_protocol::ActorKind::Human
        {
            return Err(Error::Unauthorized);
        }
        tx.commit()?;
        Ok(())
    }
}
