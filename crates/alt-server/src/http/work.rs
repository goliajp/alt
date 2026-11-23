//! What a request is authorized as, and what it runs.
use super::*;
use alt_server_store::{Credential, PackView};

/// A request once its caller is known: who that is, and the body it may send.
pub(super) struct Prepared {
    pub(super) credential: Credential,
    /// the account of the caller
    pub(super) caller: String,
    pub(super) size: u64,
    pub(super) view: Option<PackView>,
}

/// Authenticates the caller for the route and bounds the body it may send. A Git
/// request gets its repository view from the same statement.
pub(super) fn prepare(
    factory: &pool::Pool,
    route: &Route,
    options: &NativeOptions,
    token: &str,
) -> alt_server_store::Result<Prepared> {
    let mut store = factory.open()?;
    let (credential, view) = match route {
        Route::GitReceive(repository)
        | Route::GitUpload(repository)
        | Route::GitAdvertise(repository, _) => {
            let (credential, view) =
                factory.git_open(&mut store, token, repository, crate::git::ref_filter(route))?;
            (credential, Some(view))
        }
        _ => (factory.authenticate(&mut store, token)?, None),
    };
    let size = match route {
        Route::ChangeLink(route) => {
            crate::change_links::api::request_bytes(&mut store, &credential, route)?
        }
        Route::Portable(route) => {
            crate::portable::request_bytes(&mut store, &credential, options, route)?
        }
        Route::IssueAgent => 16_384,
        Route::GitReceive(_) => {
            if !view.as_ref().is_some_and(|view| view.writable) {
                return Err(Error::Unauthorized);
            }
            store
                .limits()
                .total_bytes
                .checked_add(store.limits().request_bytes as u64)
                .ok_or(Error::Limit("Git body bytes"))?
        }
        Route::GitUpload(_) => store.limits().request_bytes as u64,
        Route::GitAdvertise(..) => 0,
        Route::Request(key, RequestAction::Upload(digest)) => {
            let entries = store.selected_upload(&credential, key, digest)?;
            entries[0].bytes
        }
        Route::Request(key, RequestAction::UploadChunk(digest)) => {
            store.selected_chunk(&credential, key, digest)?
        }
        Route::Begin(scope) => {
            let info = store.repository_info(&credential, &scope.repository_instance_id)?;
            if info.scope != *scope {
                return Err(Error::Unauthorized);
            }
            store.limits().request_bytes as u64
        }
        Route::Request(_, RequestAction::Approve) => store.limits().request_bytes as u64,
        _ => 0,
    };
    // whose bodies are counted together: the account, whatever token it sends
    let caller = match &view {
        Some(view) => view.actor_id.clone(),
        None => store.principal(&credential)?.actor_id,
    };
    Ok(Prepared {
        credential,
        caller,
        size,
        view,
    })
}
/// what the execution of an authorized request needs besides its body
pub(super) struct Work {
    pub(super) factory: pool::Pool,
    pub(super) credential: Credential,
    pub(super) token: String,
    pub(super) options: NativeOptions,
    pub(super) metrics: Arc<alt_server_telemetry::Metrics>,
    pub(super) compaction: crate::compaction::Compaction,
    pub(super) protocol_v2: bool,
    pub(super) view: Option<PackView>,
    pub(super) verifying: crate::git::Verifying,
}
impl Work {
    pub(super) fn run(
        self,
        store: &mut Store,
        route: Route,
        source: &mut std::fs::File,
    ) -> alt_server_store::Result<Output> {
        match route {
            Route::Metrics => observation::metrics(store, &self.credential, &self.metrics),
            Route::ChangeLink(route) => {
                let current = self.factory.authenticate(store, &self.token)?;
                crate::change_links::api::execute(
                    store,
                    &current,
                    &self.factory.change_links,
                    route,
                    source,
                )
            }
            Route::Portable(route) => {
                let current = self.factory.authenticate(store, &self.token)?;
                crate::portable::execute(
                    store,
                    &current,
                    &self.options,
                    route,
                    source,
                    &mut |store| self.factory.authenticate(store, &self.token),
                )
            }
            Route::GitAdvertise(..) | Route::GitUpload(..) | Route::GitReceive(..) => {
                let pushed = match &route {
                    Route::GitReceive(repository) => Some(repository.clone()),
                    _ => None,
                };
                let output = crate::git::execute_git_with_cache(
                    store,
                    &self.credential,
                    route,
                    self.protocol_v2,
                    source,
                    self.view.ok_or(Error::Integrity("Git repository view"))?,
                    Some(self.verifying),
                )?;
                if let Some(repository) = pushed {
                    self.compaction.published(&repository);
                }
                Ok(output)
            }
            _ => {
                // a native publication stores its Git objects as a segment too
                let published = match &route {
                    Route::Request(key, _) => Some(key.scope.repository_instance_id.clone()),
                    _ => None,
                };
                let output = execute_native(store, &self.credential, &self.options, route, source)?;
                if let Some(repository) = published {
                    self.compaction.published(&repository);
                }
                Ok(output)
            }
        }
    }
}
