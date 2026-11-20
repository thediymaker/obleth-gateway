//! The models and endpoints `/fairshare/live` reads to work out capacity and
//! which pools have servers. The dashboard polls that route every 2 s from
//! every open tab; reading (and decrypting) both tables each time was most of
//! its cost. One copy is kept for [`LIVE_CATALOG_TTL`], refreshed by the first
//! request after it ages out while concurrent requests wait for that read.

use std::sync::Arc;
use std::time::{Duration, Instant};

use obleth_config::{ModelEndpoint, ModelRoute};
use obleth_store::Store;

/// How stale the live view's model list may be. A model enabled, disabled
/// or renamed on another replica shows there within this long; on the
/// replica that made the change, at once (see [`LiveCatalog::invalidate`]).
pub const LIVE_CATALOG_TTL: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub struct LiveCatalogSnapshot {
    pub models: Vec<ModelRoute>,
    pub endpoints: Vec<ModelEndpoint>,
}

#[derive(Debug, Default)]
pub struct LiveCatalog {
    cached: tokio::sync::Mutex<Option<(Instant, Arc<LiveCatalogSnapshot>)>>,
}

impl LiveCatalog {
    /// The cached copy while it is younger than the TTL, else a fresh read.
    /// A failed read is returned and nothing is cached.
    pub async fn get(
        &self,
        store: &Store,
    ) -> Result<Arc<LiveCatalogSnapshot>, obleth_store::StoreError> {
        let mut cached = self.cached.lock().await;
        if let Some((at, snap)) = cached.as_ref() {
            if at.elapsed() < LIVE_CATALOG_TTL {
                return Ok(snap.clone());
            }
        }
        let (models, endpoints) =
            tokio::try_join!(store.list_models(), store.all_model_endpoints())?;
        let snap = Arc::new(LiveCatalogSnapshot { models, endpoints });
        *cached = Some((Instant::now(), snap.clone()));
        Ok(snap)
    }

    /// Drop the cached copy so the next read sees a write this replica just
    /// made.
    pub async fn invalidate(&self) {
        *self.cached.lock().await = None;
    }
}
