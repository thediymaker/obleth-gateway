use crate::admin::AdminClient;

pub struct Snapshot {
    pub models: Vec<String>,
    pub tenants: Vec<String>,
    pub capacity: u32,
    pub in_flight: u64,
    pub queued: u64,
}

pub async fn fetch(admin: &AdminClient) -> Snapshot {
    let live = admin.fairshare_live().await.ok();
    Snapshot {
        models: admin.list_model_names().await.unwrap_or_default(),
        tenants: admin.list_tenant_names().await.unwrap_or_default(),
        capacity: admin.get_capacity().await.unwrap_or(0),
        in_flight: live.as_ref().map(|l| l.global_in_flight).unwrap_or(0),
        queued: live.map(|l| l.global_queued).unwrap_or(0),
    }
}
