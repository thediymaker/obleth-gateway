#[derive(Clone, Debug, PartialEq)]
pub struct KeyPlan {
    /// Existing key id to reuse, if any. Produced by `plan_key`; the runtime
    /// call site (`admin.rs`) only consults `prune` and `mint` — the gateway API
    /// does not expose the secret for a reused key. Field kept for test assertions.
    #[allow(dead_code)]
    pub reuse: Option<String>,
    /// Extra same-named keys for this tenant to delete (avoid sprawl).
    pub prune: Vec<String>,
    /// Mint a fresh key (only when none can be reused).
    pub mint: bool,
}

/// Decide what to do with the obench demo key for a tenant. Reuse the first
/// match, prune the rest, mint only when absent. Keeps repeated runs from
/// accumulating hundreds of test keys.
pub fn plan_key(
    existing: &[(String, String, String)],
    tenant_id: &str,
    key_name: &str,
) -> KeyPlan {
    let mut matches: Vec<&String> = existing
        .iter()
        .filter(|(_, tid, name)| tid == tenant_id && name == key_name)
        .map(|(id, _, _)| id)
        .collect();

    if let Some(reuse) = matches.first().cloned() {
        let prune = matches.split_off(1).into_iter().cloned().collect();
        KeyPlan { reuse: Some(reuse.clone()), prune, mint: false }
    } else {
        KeyPlan { reuse: None, prune: Vec::new(), mint: true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelAction {
    Create,
    Update(String),
}

pub fn plan_model(existing_id: Option<&str>) -> ModelAction {
    match existing_id {
        Some(id) => ModelAction::Update(id.to_string()),
        None => ModelAction::Create,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(id: &str, tid: &str, name: &str) -> (String, String, String) {
        (id.into(), tid.into(), name.into())
    }

    #[test]
    fn mints_when_no_key_exists() {
        let plan = plan_key(&[], "t1", "obench");
        assert_eq!(plan, KeyPlan { reuse: None, prune: vec![], mint: true });
    }

    #[test]
    fn reuses_single_existing_key() {
        let inv = vec![k("k1", "t1", "obench")];
        let plan = plan_key(&inv, "t1", "obench");
        assert_eq!(plan, KeyPlan { reuse: Some("k1".into()), prune: vec![], mint: false });
    }

    #[test]
    fn reuses_first_and_prunes_duplicates() {
        let inv = vec![k("k1", "t1", "obench"), k("k2", "t1", "obench"), k("k3", "t2", "obench")];
        let plan = plan_key(&inv, "t1", "obench");
        assert_eq!(plan.reuse, Some("k1".into()));
        assert_eq!(plan.prune, vec!["k2".to_string()]);
        assert!(!plan.mint);
    }

    #[test]
    fn ignores_other_tenants_and_names() {
        let inv = vec![k("k1", "other", "obench"), k("k2", "t1", "different")];
        assert_eq!(plan_key(&inv, "t1", "obench").mint, true);
    }

    #[test]
    fn model_action_create_vs_update() {
        assert_eq!(plan_model(None), ModelAction::Create);
        assert_eq!(plan_model(Some("m9")), ModelAction::Update("m9".into()));
    }
}
