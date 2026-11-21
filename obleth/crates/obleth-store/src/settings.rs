//! Settings kept in `app_settings`.

use super::*;

/// The `capacity` row of `app_settings`: the global in-flight ceiling saved
/// from the dashboard.
#[derive(serde::Serialize, serde::Deserialize)]
struct GlobalCeiling {
    max_in_flight: usize,
}

impl Store {
    /// One `app_settings` row decoded as `T`, or `None` when it is unset.
    async fn get_setting<T>(&self, key: &str) -> Result<Option<T>>
    where
        T: serde::de::DeserializeOwned + Send + Unpin + 'static,
    {
        let value: Option<sqlx::types::Json<T>> =
            sqlx::query_scalar("select value from app_settings where key = $1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(value.map(|v| v.0))
    }

    /// Write one `app_settings` row (upsert on `key`).
    async fn put_setting<T: serde::Serialize + Sync>(&self, key: &str, value: &T) -> Result<()> {
        sqlx::query(
            "insert into app_settings (key, value, updated_at)
             values ($1, $2, now())
             on conflict (key) do update set value = excluded.value, updated_at = now()",
        )
        .bind(key)
        .bind(sqlx::types::Json(value))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load the persisted alert settings, or `None` if none have been saved.
    pub async fn get_alert_settings(&self) -> Result<Option<obleth_config::AlertSettings>> {
        self.get_setting("alerts").await
    }

    /// Persist the alert settings.
    pub async fn put_alert_settings(&self, settings: &obleth_config::AlertSettings) -> Result<()> {
        self.put_setting("alerts", settings).await
    }

    /// The global in-flight ceiling saved from the dashboard, or `None` when
    /// none has been saved (`OBLETH_GLOBAL_MAX_IN_FLIGHT` applies).
    pub async fn get_global_ceiling(&self) -> Result<Option<usize>> {
        Ok(self
            .get_setting::<GlobalCeiling>("capacity")
            .await?
            .map(|c| c.max_in_flight.max(1)))
    }

    /// Save the global in-flight ceiling.
    pub async fn put_global_ceiling(&self, max_in_flight: usize) -> Result<()> {
        self.put_setting("capacity", &GlobalCeiling { max_in_flight })
            .await
    }

    /// Forget the saved ceiling, so `OBLETH_GLOBAL_MAX_IN_FLIGHT` applies.
    pub async fn delete_global_ceiling(&self) -> Result<()> {
        sqlx::query("delete from app_settings where key = 'capacity'")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Load the advice state (snoozed findings), or `None` if never saved.
    /// Stored as plain JSON: the admin crate owns its shape.
    pub async fn get_advice_state(&self) -> Result<Option<serde_json::Value>> {
        self.get_setting("advice").await
    }

    /// Persist the advice state.
    pub async fn put_advice_state(&self, state: &serde_json::Value) -> Result<()> {
        self.put_setting("advice", state).await
    }

    /// Load the persisted `auto` router settings, or `None` if unset.
    pub async fn get_auto_router_settings(
        &self,
    ) -> Result<Option<obleth_config::AutoRouterSettings>> {
        self.get_setting("auto_router").await
    }

    /// Persist the `auto` router settings.
    pub async fn put_auto_router_settings(
        &self,
        settings: &obleth_config::AutoRouterSettings,
    ) -> Result<()> {
        self.put_setting("auto_router", settings).await
    }

    /// Load the persisted model-"boons" settings, or `None` if unset.
    pub async fn get_boon_settings(&self) -> Result<Option<obleth_config::BoonSettings>> {
        self.get_setting("boons").await
    }

    /// Persist the model-"boons" settings.
    pub async fn put_boon_settings(&self, settings: &obleth_config::BoonSettings) -> Result<()> {
        self.put_setting("boons", settings).await
    }

    /// Whether the Charo control-plane assistant is enabled in the dashboard UI.
    /// `None` means unset (callers should treat this as enabled by default).
    pub async fn get_charo_enabled(&self) -> Result<Option<bool>> {
        self.get_setting("charo_enabled").await
    }

    /// Persist the Charo assistant UI toggle (the legacy `charo_enabled` key).
    pub async fn set_charo_enabled(&self, enabled: bool) -> Result<()> {
        self.put_setting("charo_enabled", &enabled).await
    }

    /// Load the full Charo settings blob. Falls back to the legacy `charo_enabled`
    /// boolean (with defaults for the newer fields) when `charo_settings` is unset,
    /// so upgrades preserve the operator's existing show/hide choice.
    pub async fn get_charo_settings(&self) -> Result<obleth_config::CharoSettings> {
        if let Some(settings) = self.get_setting("charo_settings").await? {
            return Ok(settings);
        }
        // Legacy fallback: synthesize from the old boolean key.
        let enabled = self.get_charo_enabled().await?.unwrap_or(true);
        Ok(obleth_config::CharoSettings {
            enabled,
            ..Default::default()
        })
    }

    /// Persist the full Charo settings blob. Also keeps the legacy
    /// `charo_enabled` key in sync, so a rollback to a release that reads
    /// only that key keeps the operator's choice.
    pub async fn set_charo_settings(&self, settings: &obleth_config::CharoSettings) -> Result<()> {
        self.put_setting("charo_settings", settings).await?;
        self.set_charo_enabled(settings.enabled).await
    }

    /// The saved key prefix (`app_settings.key_prefix`), or `None` when none
    /// is saved. A saved value that is no longer a valid prefix reads as none.
    pub async fn get_key_prefix_setting(&self) -> Result<Option<String>> {
        let value: Option<serde_json::Value> = self.get_setting("key_prefix").await?;
        Ok(value
            .as_ref()
            .and_then(|v| v.get("prefix"))
            .and_then(|p| p.as_str())
            .filter(|p| obleth_config::is_valid_key_prefix(p))
            .map(str::to_string))
    }

    /// Save the prefix new keys start with. The caller validates it.
    pub async fn put_key_prefix_setting(&self, prefix: &str) -> Result<()> {
        self.put_setting("key_prefix", &serde_json::json!({ "prefix": prefix }))
            .await
    }

    /// The prefix new keys start with: the saved setting, else
    /// `OBLETH_KEY_PREFIX`, else `obl`.
    pub async fn key_prefix(&self) -> Result<String> {
        Ok(self
            .get_key_prefix_setting()
            .await?
            .unwrap_or_else(|| obleth_config::default_key_prefix().to_string()))
    }

    /// The named networks (`app_settings.named_networks`), or none. Rows
    /// may be groups or the older one-range rows (merged by name); anything
    /// that no longer validates is skipped as it is read, without hiding the
    /// rest.
    pub async fn named_networks(&self) -> Result<Vec<obleth_config::network::NamedNetwork>> {
        let Some(value) = self
            .get_setting::<serde_json::Value>("named_networks")
            .await?
        else {
            return Ok(Vec::new());
        };
        let saved: Vec<obleth_config::network::NamedNetwork> = value
            .get("networks")
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| serde_json::from_value(r.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        Ok(obleth_config::network::read_named_networks(&saved))
    }

    /// Save the named networks. The caller validates them.
    pub async fn put_named_networks(
        &self,
        networks: &[obleth_config::network::NamedNetwork],
    ) -> Result<()> {
        self.put_setting(
            "named_networks",
            &serde_json::json!({ "networks": networks }),
        )
        .await
    }

    /// Load the persisted raw-usage retention setting, or `None` if unset.
    pub async fn get_usage_retention_settings(
        &self,
    ) -> Result<Option<obleth_config::UsageRetentionSettings>> {
        self.get_setting("usage_retention").await
    }

    /// Persist the raw-usage retention setting.
    pub async fn put_usage_retention_settings(
        &self,
        settings: &obleth_config::UsageRetentionSettings,
    ) -> Result<()> {
        self.put_setting("usage_retention", settings).await
    }

    /// Load the system-wide Slurm settings, or `None` if never configured. The
    /// stored JWT and Hugging Face token are decrypted transparently
    /// (legacy/empty values pass through).
    pub async fn get_slurm_settings(&self) -> Result<Option<obleth_config::SlurmSettings>> {
        let Some(mut settings) = self
            .get_setting::<obleth_config::SlurmSettings>("slurm")
            .await?
        else {
            return Ok(None);
        };
        if !settings.slurm_jwt.is_empty() {
            settings.slurm_jwt = cipher().decrypt(&settings.slurm_jwt)?;
        }
        if !settings.hf_token.is_empty() {
            settings.hf_token = cipher().decrypt(&settings.hf_token)?;
        }
        Ok(Some(settings))
    }

    /// Persist the system-wide Slurm settings. The JWT and Hugging Face token
    /// are encrypted at rest with the same envelope cipher used for upstream
    /// provider keys before they are written.
    pub async fn put_slurm_settings(&self, settings: &obleth_config::SlurmSettings) -> Result<()> {
        let mut to_store = settings.clone();
        if !to_store.slurm_jwt.is_empty() {
            to_store.slurm_jwt = cipher().encrypt(&to_store.slurm_jwt);
        }
        if !to_store.hf_token.is_empty() {
            to_store.hf_token = cipher().encrypt(&to_store.hf_token);
        }
        self.put_setting("slurm", &to_store).await
    }

    /// Load the persisted energy-accounting settings, or `None` if unset.
    pub async fn get_energy_settings(&self) -> Result<Option<obleth_config::EnergySettings>> {
        self.get_setting("energy").await
    }

    /// Persist the energy-accounting settings.
    pub async fn put_energy_settings(
        &self,
        settings: &obleth_config::EnergySettings,
    ) -> Result<()> {
        self.put_setting("energy", settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::serial;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn named_networks_round_trip_and_skip_bad_rows() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let before = store.named_networks().await.expect("read");
        use obleth_config::network::NamedNetwork;
        let nets = vec![
            NamedNetwork {
                name: "Campus".into(),
                ranges: vec!["10.0.0.0/8".into(), "203.0.113.0/24".into()],
                ours: true,
            },
            NamedNetwork {
                name: "Cluster pods".into(),
                ranges: vec!["10.42.0.0/16".into()],
                ours: false,
            },
        ];
        store.put_named_networks(&nets).await.expect("save");
        assert_eq!(store.named_networks().await.unwrap(), nets);
        // Older one-range rows merge by name; a row edited by hand into
        // junk is skipped, not an error, and hides none of the others.
        sqlx::query("update app_settings set value = $1 where key = 'named_networks'")
            .bind(sqlx::types::Json(serde_json::json!({ "networks": [
                { "network": "nope", "name": "x" },
                42,
                { "network": "10.0.0.0/8", "name": "Campus" },
                { "network": "203.0.113.0/24", "name": "campus" }
            ] })))
            .execute(&store.pool)
            .await
            .expect("junk");
        assert_eq!(
            store.named_networks().await.unwrap(),
            vec![NamedNetwork {
                ours: false,
                ..nets[0].clone()
            }]
        );
        store.put_named_networks(&before).await.expect("restore");
    }

    #[tokio::test]
    async fn slurm_settings_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        let settings = obleth_config::SlurmSettings {
            enabled: true,
            slurmrestd_url: "http://slurm:6820".into(),
            slurmrestd_api_version: "v0.0.40".into(),
            slurm_user: "obleth".into(),
            slurm_jwt: "header.payload.sig-secret".into(),
            node_aliases: vec![obleth_config::NodeAlias {
                host: "node001".into(),
                ip: "10.0.0.1".into(),
            }],
            cluster_defaults: obleth_config::ClusterDefaults {
                cache_dir: "/scratch/hf".into(),
                images: [("vllm".to_string(), "vllm.sif".to_string())].into(),
                ..Default::default()
            },
            hf_token: "hf_secret_token".into(),
        };
        store.put_slurm_settings(&settings).await.expect("put");

        // get round-trips the JWT back to plaintext
        let got = store
            .get_slurm_settings()
            .await
            .expect("get")
            .expect("present");
        assert!(got.enabled);
        assert_eq!(got.slurmrestd_url, settings.slurmrestd_url);
        assert_eq!(got.slurm_user, settings.slurm_user);
        assert_eq!(got.slurm_jwt, settings.slurm_jwt);
        assert_eq!(got.node_aliases, settings.node_aliases);
        assert_eq!(got.cluster_defaults, settings.cluster_defaults);
        assert_eq!(got.hf_token, settings.hf_token);

        // the JWT must be ciphertext at rest whenever a cipher is configured
        let raw: sqlx::types::Json<serde_json::Value> =
            sqlx::query("select value from app_settings where key = 'slurm'")
                .fetch_one(&store.pool)
                .await
                .expect("raw")
                .try_get("value")
                .expect("col");
        let stored_jwt = raw
            .0
            .get("slurm_jwt")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let cipher_on = std::env::var("OBLETH_ENCRYPTION_KEY")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if cipher_on {
            assert!(
                stored_jwt.starts_with("enc:v1:"),
                "jwt must be encrypted at rest, got {stored_jwt}"
            );
            assert_ne!(stored_jwt, settings.slurm_jwt);
            let stored_hf = raw
                .0
                .get("hf_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            assert!(
                stored_hf.starts_with("enc:v1:"),
                "hf_token must be encrypted at rest"
            );
        }
    }

    /// The settings helpers read back what they wrote, a second write
    /// replaces the first, and the saved ceiling can be cleared.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn settings_and_the_global_ceiling_round_trip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let before = store.get_global_ceiling().await.unwrap();

        let key = format!("test_setting_{}", Uuid::new_v4().simple());
        assert_eq!(store.get_setting::<Vec<u32>>(&key).await.unwrap(), None);
        store.put_setting(&key, &vec![1u32, 2]).await.unwrap();
        store.put_setting(&key, &vec![3u32]).await.unwrap();
        assert_eq!(
            store.get_setting::<Vec<u32>>(&key).await.unwrap(),
            Some(vec![3])
        );
        sqlx::query("delete from app_settings where key = $1")
            .bind(&key)
            .execute(&store.pool)
            .await
            .unwrap();

        store.put_global_ceiling(512).await.unwrap();
        let saved = store.get_global_ceiling().await.unwrap();
        store.delete_global_ceiling().await.unwrap();
        let cleared = store.get_global_ceiling().await.unwrap();
        if let Some(n) = before {
            store.put_global_ceiling(n).await.unwrap();
        }
        assert_eq!(saved, Some(512));
        assert_eq!(cleared, None);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Verifies that energy settings round-trip through the store via the
    /// `app_settings` upsert pattern.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn energy_settings_round_trip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        // Clean up any leftover key from a prior run.
        sqlx::query("delete from app_settings where key = 'energy'")
            .execute(&store.pool)
            .await
            .ok();

        assert!(
            store.get_energy_settings().await.unwrap().is_none(),
            "must return None before any put"
        );
        let s = obleth_config::EnergySettings {
            enabled: true,
            prometheus_url: "http://prom:9090".into(),
            power_query: "node_power_watts".into(),
            model_power_query: "sum by (model) (model_power_watts)".into(),
            model_power_label: "model".into(),
            model_requests_query: "sum by (model) (running_requests)".into(),
            poll_interval_secs: 30,
            energy_cost_per_kwh: 0.12,
            carbon_g_per_kwh: 400.0,
            pue: 1.2,
        };
        store.put_energy_settings(&s).await.expect("put");
        assert_eq!(
            store.get_energy_settings().await.expect("get"),
            Some(s.clone()),
            "get must return the stored settings"
        );

        // Upsert (overwrite) must also work.
        let s2 = obleth_config::EnergySettings {
            enabled: false,
            pue: 1.5,
            ..s
        };
        store.put_energy_settings(&s2).await.expect("put again");
        assert_eq!(
            store.get_energy_settings().await.expect("get2"),
            Some(s2),
            "second put must overwrite the first"
        );

        // Tidy up so other tests see a clean state.
        sqlx::query("delete from app_settings where key = 'energy'")
            .execute(&store.pool)
            .await
            .ok();
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Verifies the full `charo_settings` blob round-trips and that the legacy
    /// `charo_enabled` boolean stays in sync on write, plus the fallback path
    /// when only the legacy key is present.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn charo_settings_round_trip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        // Clean up any leftover keys from a prior run.
        sqlx::query("delete from app_settings where key in ('charo_settings', 'charo_enabled')")
            .execute(&store.pool)
            .await
            .ok();

        // No keys set at all: get_charo_settings falls back to full defaults.
        let defaults = store.get_charo_settings().await.expect("get default");
        assert_eq!(defaults, obleth_config::CharoSettings::default());

        // Legacy-only fallback: charo_enabled set, charo_settings absent.
        store.set_charo_enabled(false).await.expect("set legacy");
        let fallback = store.get_charo_settings().await.expect("get fallback");
        assert!(!fallback.enabled);
        assert_eq!(
            fallback.brain_model,
            obleth_config::CharoSettings::default().brain_model
        );

        let s = obleth_config::CharoSettings {
            brain_model: Some("llama-3".into()),
            bench_max_concurrency: 24,
            ..obleth_config::CharoSettings::default()
        };
        store.set_charo_settings(&s).await.expect("set");
        let got = store.get_charo_settings().await.expect("get");
        assert_eq!(got.brain_model.as_deref(), Some("llama-3"));
        assert_eq!(got.bench_max_concurrency, 24);
        // Legacy key stays in sync.
        assert_eq!(store.get_charo_enabled().await.unwrap(), Some(true));

        // Tidy up so other tests see a clean state.
        sqlx::query("delete from app_settings where key in ('charo_settings', 'charo_enabled')")
            .execute(&store.pool)
            .await
            .ok();
    }
}
