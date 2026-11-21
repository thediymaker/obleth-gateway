//! Energy settings and the energy query test.

use super::*;

/// View of the persisted energy-accounting settings.
#[derive(Debug, Serialize, ToSchema)]
pub struct EnergySettingsView {
    pub enabled: bool,
    pub prometheus_url: String,
    pub power_query: String,
    pub model_power_query: String,
    pub model_power_label: String,
    pub model_requests_query: String,
    pub poll_interval_secs: u64,
    pub energy_cost_per_kwh: f64,
    pub carbon_g_per_kwh: f64,
    pub pue: f64,
}

impl EnergySettingsView {
    fn from_settings(s: &obleth_config::EnergySettings) -> Self {
        EnergySettingsView {
            enabled: s.enabled,
            prometheus_url: s.prometheus_url.clone(),
            power_query: s.power_query.clone(),
            model_power_query: s.model_power_query.clone(),
            model_power_label: s.model_label().to_string(),
            model_requests_query: s.model_requests_query.clone(),
            poll_interval_secs: s.poll_interval_secs,
            energy_cost_per_kwh: s.energy_cost_per_kwh,
            carbon_g_per_kwh: s.carbon_g_per_kwh,
            pue: s.pue,
        }
    }
}

#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateEnergySettings {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Base URL of the operator's Prometheus. Omit to leave unchanged.
    #[serde(default)]
    pub prometheus_url: Option<String>,
    /// PromQL returning one power series (watts) per node. Omit to leave unchanged.
    #[serde(default)]
    pub power_query: Option<String>,
    /// PromQL returning one power series (watts) per model. Omit to leave
    /// unchanged; empty turns measured per-model power off.
    #[serde(default)]
    pub model_power_query: Option<String>,
    /// The label naming the model in `model_power_query`'s result. Omit to
    /// leave unchanged; empty resets it to `model`.
    #[serde(default)]
    pub model_power_label: Option<String>,
    /// PromQL returning requests in progress per model on its servers.
    /// Omit to leave unchanged; empty counts only the gateway's requests.
    #[serde(default)]
    pub model_requests_query: Option<String>,
    /// Poll cadence in seconds. Omit/zero leaves unchanged.
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
    /// Electricity rate (USD/kWh). Omit to leave unchanged.
    #[serde(default)]
    pub energy_cost_per_kwh: Option<f64>,
    /// Grid carbon intensity (gCO2/kWh). Omit to leave unchanged.
    #[serde(default)]
    pub carbon_g_per_kwh: Option<f64>,
    /// Facility overhead multiplier. Omit or non-positive leaves unchanged.
    #[serde(default)]
    pub pue: Option<f64>,
}

fn merge_energy_settings(
    existing: &obleth_config::EnergySettings,
    body: &UpdateEnergySettings,
) -> Result<obleth_config::EnergySettings> {
    Ok(obleth_config::EnergySettings {
        enabled: body.enabled.unwrap_or(existing.enabled),
        prometheus_url: body
            .prometheus_url
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| existing.prometheus_url.clone()),
        power_query: body
            .power_query
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| existing.power_query.clone()),
        model_power_query: body
            .model_power_query
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| existing.model_power_query.clone()),
        model_power_label: body
            .model_power_label
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| existing.model_power_label.clone()),
        model_requests_query: body
            .model_requests_query
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| existing.model_requests_query.clone()),
        poll_interval_secs: at_least(
            "poll_interval_secs",
            body.poll_interval_secs,
            1,
            existing.poll_interval_secs,
        )?,
        energy_cost_per_kwh: at_least(
            "energy_cost_per_kwh",
            body.energy_cost_per_kwh,
            0.0,
            existing.energy_cost_per_kwh,
        )?,
        carbon_g_per_kwh: at_least(
            "carbon_g_per_kwh",
            body.carbon_g_per_kwh,
            0.0,
            existing.carbon_g_per_kwh,
        )?,
        // PUE is total facility power over IT power: 1.0 is the physical floor.
        pue: at_least("pue", body.pue, 1.0, existing.pue)?,
    })
}

#[utoipa::path(
    get, path = "/api/v1/settings/energy", tag = "settings",
    responses((status = 200, body = EnergySettingsView))
)]
pub(crate) async fn get_energy_settings(
    State(state): State<AdminState>,
) -> Result<Json<EnergySettingsView>> {
    let settings = state.store.get_energy_settings().await?.unwrap_or_default();
    Ok(Json(EnergySettingsView::from_settings(&settings)))
}

#[utoipa::path(
    put, path = "/api/v1/settings/energy", tag = "settings",
    request_body = UpdateEnergySettings,
    responses((status = 200, body = EnergySettingsView))
)]
pub(crate) async fn put_energy_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateEnergySettings>,
) -> Result<Json<EnergySettingsView>> {
    let existing = state.store.get_energy_settings().await?.unwrap_or_default();
    // The poller queries this URL from the gateway; apply the same destination
    // policy the test route enforces so a save can't bypass it.
    if let Some(url) = body.prometheus_url.as_deref().map(str::trim) {
        if !url.is_empty() {
            state.ssrf.validate(url).await?;
        }
    }
    let settings = merge_energy_settings(&existing, &body)?;
    state.store.put_energy_settings(&settings).await?;
    state
        .audit(
            &headers,
            "set_energy_settings",
            "settings",
            "energy",
            serde_json::json!({
                "enabled": settings.enabled,
                "prometheus_url": settings.prometheus_url,
                "power_query": settings.power_query,
                "model_power_query": settings.model_power_query,
                "model_power_label": settings.model_label(),
                "model_requests_query": settings.model_requests_query,
                "poll_interval_secs": settings.poll_interval_secs,
                "energy_cost_per_kwh": settings.energy_cost_per_kwh,
                "carbon_g_per_kwh": settings.carbon_g_per_kwh,
                "pue": settings.pue,
            }),
        )
        .await?;
    Ok(Json(EnergySettingsView::from_settings(&settings)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TestEnergyQuery {
    pub prometheus_url: String,
    /// The per-node query. Optional when `model_power_query` is given.
    #[serde(default)]
    pub power_query: String,
    /// The per-model query.
    #[serde(default)]
    pub model_power_query: String,
    /// The label naming the model in the per-model query's result.
    #[serde(default)]
    pub model_power_label: String,
    /// Requests in progress per model on its servers, labelled the same way.
    #[serde(default)]
    pub model_requests_query: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnergyTestResult {
    /// Sum of the per-node query; 0 when it was not given.
    pub cluster_watts: f64,
    /// Series the per-node query returned; 0 when it was not given.
    pub node_count: u64,
    /// What the per-model query returned, when it was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<ModelPowerTest>,
}

/// The per-model query's result, matched against the registered models.
#[derive(Debug, Serialize, ToSchema)]
pub struct ModelPowerTest {
    pub total_watts: f64,
    pub series: usize,
    /// Registered models that match a series, as the gateway would charge them.
    pub matched: Vec<ModelPowerMatch>,
    /// Series no registered model matches.
    pub unmatched_series: Vec<String>,
    /// Requests in progress across every series of the requests query, when
    /// it was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests_in_progress: Option<f64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ModelPowerMatch {
    pub model: String,
    pub series: String,
    pub watts: f64,
}

fn model_power_test(
    series: &std::collections::BTreeMap<String, f64>,
    models: &[ModelRoute],
) -> ModelPowerTest {
    let mut used = std::collections::BTreeSet::new();
    let mut matched: Vec<ModelPowerMatch> = models
        .iter()
        .filter_map(|m| {
            let (name, watts) = crate::energy_probe::match_power_series(
                series,
                &m.model_name,
                m.capacity_service.as_deref(),
                &m.upstream_model,
            )?;
            used.insert(name.clone());
            Some(ModelPowerMatch {
                model: m.model_name.clone(),
                series: name.clone(),
                watts: *watts,
            })
        })
        .collect();
    matched.sort_by(|a, b| a.model.cmp(&b.model));
    ModelPowerTest {
        total_watts: series.values().sum(),
        series: series.len(),
        matched,
        unmatched_series: series
            .keys()
            .filter(|k| !used.contains(*k))
            .cloned()
            .collect(),
        requests_in_progress: None,
    }
}

#[utoipa::path(
    post, path = "/api/v1/settings/energy/test", tag = "settings",
    request_body = TestEnergyQuery,
    responses((status = 200, body = EnergyTestResult))
)]
pub(crate) async fn test_energy_query(
    State(state): State<AdminState>,
    Json(body): Json<TestEnergyQuery>,
) -> Result<Json<EnergyTestResult>> {
    state.ssrf.validate(&body.prometheus_url).await?;
    let q = body.power_query.trim();
    let mq = body.model_power_query.trim();
    if q.is_empty() && mq.is_empty() {
        return Err(AdminError::BadRequest(
            "give a per-node or a per-model power query".into(),
        ));
    }
    let base = body.prometheus_url.trim();
    let http = ssrf::upstream_client_builder()
        .build()
        .map_err(|e| AdminError::BadRequest(format!("could not initialize HTTP client: {e}")))?;
    let mut result = EnergyTestResult {
        cluster_watts: 0.0,
        node_count: 0,
        models: None,
    };
    if !q.is_empty() {
        result.cluster_watts =
            crate::energy_probe::instant_query(&http, base, &format!("sum({q})"))
                .await
                .map_err(AdminError::BadRequest)?;
        let nodes = crate::energy_probe::instant_query(&http, base, &format!("count({q})"))
            .await
            .map_err(AdminError::BadRequest)?;
        result.node_count = nodes.max(0.0) as u64;
    }
    if !mq.is_empty() {
        let label = match body.model_power_label.trim() {
            "" => "model",
            label => label,
        };
        let series = crate::energy_probe::instant_vector(&http, base, mq, label)
            .await
            .map_err(AdminError::BadRequest)?;
        let models = state.store.list_models().await?;
        let mut test = model_power_test(&series, &models);
        let rq = body.model_requests_query.trim();
        if !rq.is_empty() {
            let requests = crate::energy_probe::instant_vector(&http, base, rq, label)
                .await
                .map_err(|e| AdminError::BadRequest(format!("requests query: {e}")))?;
            test.requests_in_progress = Some(requests.values().sum());
        }
        result.models = Some(test);
    }
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::fixture_model_route;

    #[test]
    fn energy_settings_view_round_trip() {
        let s = obleth_config::EnergySettings {
            enabled: true,
            prometheus_url: "http://prom:9090".into(),
            power_query: "node_power_watts".into(),
            model_power_query: String::new(),
            model_power_label: String::new(),
            model_requests_query: String::new(),
            poll_interval_secs: 30,
            energy_cost_per_kwh: 0.12,
            carbon_g_per_kwh: 400.0,
            pue: 1.2,
        };
        let view = EnergySettingsView::from_settings(&s);
        assert!(view.enabled);
        assert_eq!(view.poll_interval_secs, 30);
        assert_eq!(view.pue, 1.2);
        assert_eq!(
            view.model_power_label, "model",
            "a blank label shows as the default"
        );
    }

    #[test]
    fn model_power_test_matches_registered_models() {
        let series: std::collections::BTreeMap<String, f64> = [
            ("svc-a".to_string(), 1000.0),
            ("up-b".to_string(), 500.0),
            ("idle".to_string(), 50.0),
        ]
        .into_iter()
        .collect();
        let mut a = fixture_model_route("a");
        a.capacity_service = Some("svc-a".into());
        let mut b = fixture_model_route("b");
        b.upstream_model = "up-b".into();
        let c = fixture_model_route("c");
        let t = model_power_test(&series, &[c, b, a]);
        assert_eq!(t.series, 3);
        assert_eq!(t.total_watts, 1550.0);
        let got: Vec<_> = t
            .matched
            .iter()
            .map(|m| (m.model.as_str(), m.series.as_str()))
            .collect();
        assert_eq!(got, vec![("a", "svc-a"), ("b", "up-b")]);
        assert_eq!(t.unmatched_series, vec!["idle".to_string()]);
    }

    #[test]
    fn update_energy_settings_merges_partials() {
        let existing = obleth_config::EnergySettings::default();
        let body = UpdateEnergySettings {
            enabled: Some(true),
            prometheus_url: Some("http://prom:9090".into()),
            power_query: Some("watts".into()),
            model_power_query: Some(" sum by (model) (w) ".into()),
            model_power_label: None,
            model_requests_query: None,
            poll_interval_secs: None,
            energy_cost_per_kwh: Some(0.15),
            carbon_g_per_kwh: None,
            pue: None,
        };
        let merged = merge_energy_settings(&existing, &body).expect("valid");
        assert!(merged.enabled);
        let bad = UpdateEnergySettings {
            pue: Some(0.5),
            ..UpdateEnergySettings::default()
        };
        assert!(
            matches!(merge_energy_settings(&existing, &bad), Err(AdminError::BadRequest(m)) if m.contains("pue"))
        );
        assert_eq!(merged.poll_interval_secs, 60); // untouched default
        assert_eq!(merged.energy_cost_per_kwh, 0.15);
        assert_eq!(merged.pue, 1.0);
        assert_eq!(merged.model_power_query, "sum by (model) (w)");
        assert_eq!(merged.model_power_label, "model"); // untouched default
    }
}
