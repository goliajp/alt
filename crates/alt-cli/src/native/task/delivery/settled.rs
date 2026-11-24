//! Integrations and deliveries that are completed, collected or abandoned.
use super::*;

impl NativeRepo<'_> {
    pub(in crate::native::task) fn integration_ref_operation_with_store(
        &self,
        store: &alt_task::Store,
        integration: &Integration,
    ) -> Res<Option<String>> {
        let _: Task = store.load(&integration.task)?;
        crate::request::parse_id(&integration.creator_request)?;
        let intent: crate::request::Intent = match super::transfer_source::bounded(
            &self
                .store
                .alt_dir
                .join("requests")
                .join(&integration.creator_request)
                .join("intent.json"),
            4 * 1024 * 1024,
        ) {
            Ok(raw) => alt_json::from_slice(&raw)?,
            Err(alt_task_transfer::Error::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if intent.repository != self.task_scope()?.repository
            || intent.request_id != integration.creator_request
        {
            return Err(refused(
                "integration source intent does not belong to this repository",
            ));
        }
        let mut found = None;
        for op in self.store.refs.oplog().ops()? {
            let Some(tx) = alt_refs::parse_tx(&op.payload)? else {
                continue;
            };
            let Some(context) = tx.context else { continue };
            let context: recovery::RefContext = alt_json::from_slice(&context)?;
            if !matches!(context.version, 1 | 2)
                || context.workspace.instance != intent.workspace
                || context.request_id.as_deref() != Some(&integration.creator_request)
            {
                continue;
            }
            if tx.changes.len() != 2 {
                continue;
            }
            let branch = tx.changes.iter().find(|c| c.name == integration.target);
            let notes = tx.changes.iter().find(|c| c.name == crate::meta::NOTES_REF);
            let matches = branch.is_some_and(|c| {
                c.old.as_ref().map(alt_delivery::render).as_deref() == Some(&integration.before)
                    && c.new.as_ref().map(alt_delivery::render).as_deref()
                        == Some(&integration.commit)
            }) && notes.is_some_and(|c| {
                c.old.as_ref().map(alt_delivery::render) == integration.notes_before
                    && c.new.as_ref().map(alt_delivery::render).as_deref()
                        == Some(&integration.notes_after)
            });
            if matches {
                if found.is_some() {
                    return Err(crate::machine::Failure::unknown(
                        "integration has multiple attributed target transactions",
                    )
                    .into());
                }
                found = Some(op.id.to_string());
            }
        }
        Ok(found)
    }
    pub(in crate::native::task) fn integration_completed(
        &self,
        store: &alt_task::Store,
        id: &str,
        integration: &Integration,
    ) -> Res<bool> {
        if self.received_record_complete(&alt_task_service::RecordKey {
            kind: alt_task_service::RecordKind::Integration,
            id: id.into(),
        })? {
            return Ok(true);
        }
        let Some(operation) = self.integration_ref_operation_with_store(store, integration)? else {
            return Ok(false);
        };
        if self.task_record_complete(integration)? {
            return Ok(true);
        }
        for (_, receipt) in store.list::<alt_task::IntegrationReceipt>()? {
            if receipt.integration == id
                && receipt.ref_operation == operation
                && self.task_record_complete(&receipt)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(in crate::native::task) fn task_integration_collect(
        &mut self,
        id: &str,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task integration-collect")?;
        let mut store = self.task_store()?;
        let integration: Integration = store.load(id)?;
        self.require_task(&store, &integration.task, false)?;
        let operation=self.integration_ref_operation_with_store(&store,&integration)?.ok_or_else(||refused("no matching attributed ref transaction; collection cannot infer application or execute the intent"))?;
        let receipt = alt_task::IntegrationReceipt {
            schema_version: 1,
            integration: id.into(),
            ref_operation: operation,
            creator_request: self.task_request()?,
        };
        let receipt_id = store.publish(&receipt)?;
        let operation =
            self.task_operation(&integration.task, "task-integration-collect", &receipt_id)?;
        emit(
            out,
            json_mode,
            json!({"integration_id":id,"receipt_id":receipt_id,"receipt":receipt,"operation_id":operation,"state":"integrated","executed_again":false}),
        )
    }
}

impl NativeRepo<'_> {
    /// Hold the original request claim while proving that an interrupted
    /// integration did not apply. A pending/running process is never guessed.
    pub(in crate::native::task) fn prepare_task_abandon(
        &mut self,
        store: &alt_task::Store,
        task_id: &str,
    ) -> Res<Option<std::fs::File>> {
        self.require_task(store, task_id, false)?;
        let integrations: Vec<_> = store
            .list::<Integration>()?
            .into_iter()
            .filter(|(_, i)| i.task == task_id)
            .collect();
        if integrations.len() > 1 {
            return Err(crate::machine::Failure::unknown(
                "multiple integration intents require inspection",
            )
            .into());
        }
        let Some((_, integration)) = integrations.first() else {
            return Ok(None);
        };
        crate::request::parse_id(&integration.creator_request)?;
        let path = self
            .store
            .alt_dir
            .join("requests")
            .join(&integration.creator_request)
            .join("lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        if alt_os::sys::flock(&file, alt_os::sys::LOCK_EX | alt_os::sys::LOCK_NB).is_err() {
            return Err(refused(
                "integration request is still active; query it before abandonment",
            ));
        }
        self.store.refresh()?;
        if self
            .integration_ref_operation_with_store(store, integration)?
            .is_some()
        {
            return Err(refused(
                "integration already applied; collect its receipt instead of abandoning it",
            ));
        }
        Ok(Some(file))
    }
    pub(in crate::native::task) fn integration_abandoned(
        &self,
        store: &alt_task::Store,
        integration: &Integration,
    ) -> Res<bool> {
        if self
            .integration_ref_operation_with_store(store, integration)?
            .is_some()
        {
            return Ok(false);
        }
        for (_, event) in store.list::<Event>()? {
            if event.task == integration.task
                && event.state == TaskState::Abandoned
                && self.task_record_complete(&event)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl NativeRepo<'_> {
    pub(in crate::native::task) fn delivery_completed(
        &self,
        store: &alt_task::Store,
        id: &str,
        delivery: &Delivery,
    ) -> Res<bool> {
        if self.task_record_complete(delivery)? {
            return Ok(true);
        }
        for (_, receipt) in store.list::<alt_task::DeliveryReceipt>()? {
            if receipt.delivery == id && self.task_record_complete(&receipt)? {
                store.verify_blob(&receipt.verification)?;
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(in crate::native::task) fn task_delivery_collect(
        &mut self,
        id: &str,
        git: &Path,
        json_mode: bool,
        out: &mut impl Write,
    ) -> Res<()> {
        self.ensure_writable("task delivery-collect")?;
        let store = self.task_store()?;
        let delivery: Delivery = store.load(id)?;
        self.require_task(&store, &delivery.task, false)?;
        let integration: Integration = store.load(&delivery.integration)?;
        if !self.integration_completed(&store, &delivery.integration, &integration)? {
            return Err(refused("integration completion is still unknown"));
        }
        let report: alt_delivery::ExportReport =
            alt_json::from_slice(&store.blob(&delivery.report)?)?;
        drop(store);
        let verification = alt_delivery::verify_existing(
            &self.store.odb,
            &report,
            Path::new(&delivery.destination),
            git,
        )
        .map_err(|e| refused(e.to_string()))?;
        let mut store = self.task_store()?;
        let artifact = store.put_blob(&alt_json::to_vec(&verification)?)?;
        let receipt = alt_task::DeliveryReceipt {
            schema_version: 1,
            delivery: id.into(),
            verification: artifact,
            creator_request: self.task_request()?,
        };
        let receipt_id = store.publish(&receipt)?;
        let operation =
            self.task_operation(&delivery.task, "task-delivery-collect", &receipt_id)?;
        emit(
            out,
            json_mode,
            json!({"delivery_id":id,"receipt_id":receipt_id,"receipt":receipt,"verification":verification,"operation_id":operation,"exported_again":false}),
        )
    }
}
