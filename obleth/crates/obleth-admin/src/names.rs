//! Display names for rows that carry only tenant and key ids: request-log
//! pages, their facets and failure groups, the per-model breakdown and the
//! live fair-share view. Two narrow reads of the ids on hand; nothing else on
//! the tenant or key rows is read or decoded.

use std::collections::{HashMap, HashSet};

use obleth_store::{KeyName, TenantName};
use uuid::Uuid;

use crate::{AdminState, Result};

/// Names of the tenants and keys asked for. Ids that no longer exist are
/// missing, so callers fall back to whatever the row itself recorded.
#[derive(Debug, Default)]
pub(crate) struct Names {
    pub tenants: HashMap<Uuid, TenantName>,
    pub keys: HashMap<Uuid, KeyName>,
}

impl Names {
    pub fn tenant_name(&self, id: &Uuid) -> Option<&str> {
        self.tenants.get(id).map(|t| t.name.as_str())
    }

    pub fn key_name(&self, id: &Uuid) -> Option<&str> {
        self.keys.get(id).map(|k| k.name.as_str())
    }
}

/// Look up the names of `tenant_ids` and `key_ids` (duplicates allowed) in
/// two concurrent reads.
pub(crate) async fn resolve_names(
    state: &AdminState,
    tenant_ids: impl IntoIterator<Item = Uuid>,
    key_ids: impl IntoIterator<Item = Uuid>,
) -> Result<Names> {
    let tenant_ids = distinct(tenant_ids);
    let key_ids = distinct(key_ids);
    let (tenants, keys) = tokio::try_join!(
        state.store.tenant_names(&tenant_ids),
        state.store.key_names(&key_ids),
    )?;
    Ok(Names {
        tenants: tenants.into_iter().map(|t| (t.id, t)).collect(),
        keys: keys.into_iter().map(|k| (k.id, k)).collect(),
    })
}

fn distinct(ids: impl IntoIterator<Item = Uuid>) -> Vec<Uuid> {
    ids.into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}
