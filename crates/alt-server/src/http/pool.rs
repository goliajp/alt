//! HTTP-owned idle connections. Worker admission bounds active work; the pool
//! retains only a bounded number of stores and never waits while holding SQL.
use super::StoreFactory;
use alt_server_store::{Result, Store};
use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};

struct Inner {
    factory: StoreFactory,
    idle: Mutex<Idle>,
    limit: usize,
}
#[derive(Default)]
struct Idle {
    stores: Vec<Store>,
    closed: bool,
}
impl Drop for Inner {
    fn drop(&mut self) {
        let stores = std::mem::take(&mut self.idle.get_mut().expect("idle stores").stores);
        if !stores.is_empty() {
            // Cancellation can bypass async shutdown. Synchronous postgres
            // clients run their own runtime while closing, so even this final
            // bounded cleanup must run outside the HTTP runtime's async task.
            std::thread::spawn(move || drop(stores));
        }
    }
}

#[derive(Clone)]
pub(super) struct Pool(Arc<Inner>);
impl Pool {
    pub fn new(factory: StoreFactory, workers: usize) -> Self {
        Self(Arc::new(Inner {
            factory,
            idle: Mutex::new(Idle::default()),
            limit: workers.min(16),
        }))
    }
    pub fn open(&self) -> Result<Lease> {
        let retained = {
            let mut idle = self.0.idle.lock().expect("idle stores");
            if idle.closed {
                return Err(std::io::Error::other("HTTP store pool closed").into());
            }
            idle.stores.pop()
        };
        let store = match retained {
            Some(mut store) => {
                // A failed checkout drops this connection. It never retries a
                // request or hides a current schema/instance/Cloud failure.
                store.check_connection()?;
                self.0.factory.check_mode(&mut store)?;
                store
            }
            None => self.0.factory.open()?,
        };
        Ok(Lease {
            store: Some(store),
            pool: self.clone(),
        })
    }
    pub async fn shutdown(&self) -> std::io::Result<()> {
        let stores = {
            let mut idle = self.0.idle.lock().expect("idle stores");
            idle.closed = true;
            std::mem::take(&mut idle.stores)
        };
        tokio::task::spawn_blocking(move || drop(stores))
            .await
            .map_err(|_| std::io::Error::other("database connection cleanup failed"))
    }
}
impl Deref for Pool {
    type Target = StoreFactory;
    fn deref(&self) -> &StoreFactory {
        &self.0.factory
    }
}

pub(super) struct Lease {
    store: Option<Store>,
    pool: Pool,
}
impl Deref for Lease {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.store.as_ref().expect("store lease")
    }
}
impl DerefMut for Lease {
    fn deref_mut(&mut self) -> &mut Store {
        self.store.as_mut().expect("store lease")
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let mut idle = self.pool.0.idle.lock().expect("idle stores");
        if !idle.closed && idle.stores.len() < self.pool.0.limit {
            idle.stores.push(self.store.take().expect("store lease"));
        }
    }
}
