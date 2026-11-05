use alt_remote_protocol::{Limits, Receipt};
use alt_repository_service::VerifiedGraph;
use alt_server_store::{Credential, Error, PublicationWork, Result, Store};
use alt_task_service::{TaskLimits, VerifiedPublication, VerifiedTaskGraph};

/// One worker's immutable verification plus the SQL adapter's live fence.
pub struct ValidatedWork {
    work: PublicationWork,
    graph: VerifiedGraph,
    tasks: Option<VerifiedTaskGraph>,
}
impl ValidatedWork {
    pub fn verify(work: PublicationWork, limits: Limits, task_limits: TaskLimits) -> Result<Self> {
        let verified = VerifiedPublication::verify(
            work.publication(),
            work.manifest(),
            work.source(),
            work.artifacts(),
            limits,
            task_limits,
        )?;
        Ok(Self {
            work,
            graph: verified.graph,
            tasks: verified.tasks,
        })
    }
    pub fn work(&self) -> &PublicationWork {
        &self.work
    }
    pub fn mark_ready(&self, store: &mut Store, credential: &Credential) -> Result<()> {
        match &self.tasks {
            Some(tasks) => store.mark_ready_with_tasks(credential, &self.work, &self.graph, tasks),
            None => store.mark_ready(credential, &self.work, &self.graph),
        }
    }
    pub fn approve(
        &self,
        store: &mut Store,
        reviewer: &Credential,
        integration: &str,
        inspected: &alt_remote_protocol::ApprovalContext,
        ttl_ms: u64,
    ) -> Result<()> {
        store.approve_integration(
            reviewer,
            &self.work,
            &self.graph,
            self.tasks
                .as_ref()
                .ok_or(Error::Precondition("task integration required"))?,
            integration,
            inspected,
            ttl_ms,
        )
    }
    pub fn publish(&self, store: &mut Store, credential: &Credential) -> Result<Receipt> {
        match &self.tasks {
            Some(tasks) => store.publish_with_tasks(credential, &self.work, &self.graph, tasks),
            None => store.publish(credential, &self.work, &self.graph),
        }
    }
}
