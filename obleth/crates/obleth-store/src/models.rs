//! Model routes and their row mapping.

use super::*;

/// Validate a caller-supplied tag list and re-serialize what survives to the
/// storage form: bare `base` when the declared level is 1 (the untiered
/// case, byte-identical to pre-tiering storage), `base:level` otherwise. Any
/// tag whose base falls outside [`obleth_config::MODEL_TAGS`] is dropped, same
/// as the old bare-only `normalize_tags` write path. Storing the suffixed
/// form (rather than the bare-only [`obleth_config::normalize_tags`] output)
/// is what lets a declared strength level survive a save. An explicit `:1` is
/// preserved rather than collapsed to the bare tag: bare means "derive my
/// level from cost rank" under `TierSource::Hybrid`, while `tag:1` pins the
/// model to the bottom tier on purpose — a distinction the save path must
/// not erase.
pub(crate) fn serialize_tag_levels<I, S>(tags: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    obleth_config::canonical_tags(tags)
}

/// Every column of `models` that [`model_from_row`] reads, for each select and
/// RETURNING clause that feeds the mapper. One list, so a write cannot hand
/// back a row that is missing a column: the mapper reads the newer columns
/// leniently, and a missing one used to come back as its default and go to
/// the data plane that way.
pub(crate) const MODEL_COLUMNS: &str = "id, model_name, description, upstream_model, api_base, api_key,
     upstream_headers, model_type, aliases, variants, lifecycle, reasoning,
     listed, rejected_request_fields, session_affinity, quantization, input_cost_per_token, output_cost_per_token,
     cost_per_image, cost_per_audio_second, cost_per_character, cost_per_video, context_window, admission_weight,
     max_in_flight, supports_function_calling, supports_system_messages, supports_response_schema, supports_tool_choice, supports_vision,
     enabled, cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
     capacity_mode, capacity_tuned_at, capacity_source, capacity_namespace, capacity_service, per_replica_max_in_flight,
     capacity_headroom, request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode, debug_diagnostics,
     energy_slots_per_node, route_bias, auto_eligible, draft_model, verify_api_base, verify_upstream_model,
     created_at, updated_at";

pub(crate) fn model_from_row(row: &PgRow) -> Result<ModelRoute> {
    Ok(ModelRoute {
        id: row.try_get("id")?,
        model_name: row.try_get("model_name")?,
        description: row.try_get("description")?,
        upstream_model: row.try_get("upstream_model")?,
        api_base: row.try_get("api_base")?,
        api_key: cipher().decrypt_opt(row.try_get("api_key")?)?,
        upstream_headers: upstream_headers_from_row(row)?,
        // Every statement that feeds this mapper selects MODEL_COLUMNS, so the
        // lenient reads below only cover rows from older schemas mid-upgrade.
        model_type: row
            .try_get::<String, _>("model_type")
            .unwrap_or_else(|_| obleth_config::DEFAULT_MODEL_TYPE.to_string()),
        input_cost_per_token: row.try_get("input_cost_per_token")?,
        output_cost_per_token: row.try_get("output_cost_per_token")?,
        cost_per_image: row.try_get("cost_per_image").unwrap_or(0.0),
        cost_per_audio_second: row.try_get("cost_per_audio_second").unwrap_or(0.0),
        cost_per_character: row.try_get("cost_per_character").unwrap_or(0.0),
        cost_per_video: row.try_get("cost_per_video").unwrap_or(0.0),
        context_window: row.try_get("context_window")?,
        admission_weight: row.try_get("admission_weight")?,
        max_in_flight: row.try_get("max_in_flight")?,
        // Tolerant reads for the capacity-mode columns: statements that don't
        // select them degrade to the static default rather than erroring.
        capacity_mode: row
            .try_get::<String, _>("capacity_mode")
            .unwrap_or_else(|_| obleth_config::DEFAULT_CAPACITY_MODE.to_string()),
        capacity_tuned_at: row.try_get("capacity_tuned_at").unwrap_or(None),
        capacity_source: row
            .try_get::<String, _>("capacity_source")
            .unwrap_or_else(|_| obleth_config::DEFAULT_CAPACITY_SOURCE.to_string()),
        capacity_namespace: row.try_get("capacity_namespace").unwrap_or(None),
        capacity_service: row.try_get("capacity_service").unwrap_or(None),
        per_replica_max_in_flight: row.try_get("per_replica_max_in_flight").unwrap_or(None),
        capacity_headroom: row.try_get("capacity_headroom").unwrap_or(1.0),
        supports_function_calling: row.try_get("supports_function_calling")?,
        supports_system_messages: row.try_get("supports_system_messages")?,
        supports_response_schema: row.try_get("supports_response_schema")?,
        supports_tool_choice: row.try_get("supports_tool_choice")?,
        // Tolerant read: statements that don't select `supports_vision` (e.g.
        // older callers) degrade to false rather than erroring.
        supports_vision: row.try_get("supports_vision").unwrap_or(false),
        enabled: row.try_get("enabled")?,
        cache_enabled: row.try_get("cache_enabled")?,
        cache_ttl_secs: row.try_get("cache_ttl_secs")?,
        // Tolerant read: SQL statements that don't select `tags` (e.g. capacity
        // toggles) degrade to an empty list rather than erroring.
        tags: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("tags")
            .map(|j| j.0)
            .unwrap_or_default(),
        // Tolerant read: statements that don't select `boons` degrade to an
        // empty list rather than erroring.
        boons: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("boons")
            .map(|j| j.0)
            .unwrap_or_default(),
        // Tolerant read: statements that don't select `tool_servers` degrade
        // to an empty list rather than erroring.
        tool_servers: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("tool_servers")
            .map(|j| j.0)
            .unwrap_or_default(),
        // Tolerant reads for the reliability columns: statements that don't
        // select them degrade to defaults rather than erroring.
        request_timeout_secs: row.try_get("request_timeout_secs").unwrap_or(None),
        max_retries: row.try_get("max_retries").unwrap_or(0),
        retry_backoff_ms: row
            .try_get("retry_backoff_ms")
            .unwrap_or(obleth_config::DEFAULT_RETRY_BACKOFF_MS),
        endpoint_selection_mode: row
            .try_get::<String, _>("endpoint_selection_mode")
            .unwrap_or_else(|_| obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string()),
        // Tolerant read: statements that don't select `debug_diagnostics`
        // degrade to false rather than erroring.
        debug_diagnostics: row.try_get("debug_diagnostics").unwrap_or(false),
        // Tolerant read: column added in the energy-accounting migration;
        // older SQL statements or pre-migration rows degrade to 0 (off).
        energy_slots_per_node: row.try_get("energy_slots_per_node").unwrap_or(0),
        // Tolerant read: column added in the route-bias migration; older SQL
        // statements or pre-migration rows degrade to the neutral 1.0 rather
        // than a score-zeroing 0.0.
        route_bias: row.try_get("route_bias").unwrap_or(1.0),
        // Tolerant read: column added in the auto-eligibility migration; older
        // SQL statements or pre-migration rows degrade to eligible, never to a
        // silent exclusion from the auto router.
        auto_eligible: row.try_get("auto_eligible").unwrap_or(true),
        // Tolerant reads: columns added in the aliases/quantization migration.
        // An alias list that failed to read degrades to empty, which costs a
        // stale resolver key at worst; a format that failed to read degrades to
        // `unknown`, never to a wrong claim about how the model is served.
        aliases: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("aliases")
            .map(|j| j.0)
            .unwrap_or_default(),
        // Tolerant read: column added in the variants migration; statements
        // that don't select it degrade to "no variants", like `aliases`.
        variants: variants_from_row(row),
        // Tolerant read, as for `variants`: a statement that doesn't select the
        // column, or a value this build can't read, is an active model.
        lifecycle: lifecycle_from_row(row),
        reasoning: reasoning_from_row(row),
        listed: listed_from_row(row),
        rejected_request_fields: rejected_request_fields_from_row(row),
        session_affinity: session_affinity_from_row(row),
        quantization: row
            .try_get::<String, _>("quantization")
            .unwrap_or_else(|_| obleth_config::DEFAULT_QUANTIZATION.to_string()),
        // Tolerant reads: columns added in the speculation-fields migration;
        // pre-migration rows degrade to "cannot speculate".
        draft_model: row.try_get("draft_model").unwrap_or_default(),
        verify_api_base: row.try_get("verify_api_base").unwrap_or_default(),
        verify_upstream_model: row.try_get("verify_upstream_model").unwrap_or_default(),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Encrypt each upstream header value for storage, like `models.api_key`.
/// Read the `variants` column, tolerating statements that do not select it.
fn variants_from_row(row: &PgRow) -> Vec<obleth_config::ModelVariant> {
    row.try_get::<sqlx::types::Json<Vec<obleth_config::ModelVariant>>, _>("variants")
        .map(|j| j.0)
        .unwrap_or_default()
}

fn family_from_row(row: &PgRow) -> Result<obleth_config::ModelFamily> {
    let default_level: String = row.try_get("default_level")?;
    Ok(obleth_config::ModelFamily {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        default_level: obleth_config::ReasoningLevel::parse(&default_level)
            .unwrap_or(obleth_config::ReasoningLevel::None),
        members: row
            .try_get::<sqlx::types::Json<obleth_config::FamilyMembers>, _>("members")
            .map(|j| j.0)
            .unwrap_or_default(),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Read the `listed` column, tolerating statements that do not select it
/// (they read as listed).
fn listed_from_row(row: &PgRow) -> bool {
    row.try_get::<bool, _>("listed").unwrap_or(true)
}

/// Read the `rejected_request_fields` column, tolerating statements that do
/// not select it (they read as refusing nothing).
fn rejected_request_fields_from_row(row: &PgRow) -> Vec<String> {
    row.try_get::<sqlx::types::Json<Vec<String>>, _>("rejected_request_fields")
        .map(|j| j.0)
        .unwrap_or_default()
}

/// Read the `session_affinity` column, tolerating statements that do not
/// select it (they read as following the gateway).
fn session_affinity_from_row(row: &PgRow) -> obleth_config::ModelSessionAffinity {
    row.try_get::<sqlx::types::Json<obleth_config::ModelSessionAffinity>, _>("session_affinity")
        .map(|j| j.0)
        .unwrap_or_default()
}

/// Read the `reasoning` column, tolerating statements that do not select it.
fn reasoning_from_row(row: &PgRow) -> obleth_config::ModelReasoning {
    row.try_get::<sqlx::types::Json<obleth_config::ModelReasoning>, _>("reasoning")
        .map(|j| j.0)
        .unwrap_or_default()
}

/// Read the `lifecycle` column, tolerating statements that do not select it.
fn lifecycle_from_row(row: &PgRow) -> obleth_config::ModelLifecycle {
    row.try_get::<sqlx::types::Json<obleth_config::ModelLifecycle>, _>("lifecycle")
        .map(|j| j.0)
        .unwrap_or_default()
}

/// Everything a model is written with, apart from the settings that have
/// their own endpoints. The Management API handler fills the defaults and
/// validates the values; the store normalizes and binds them in one
/// statement.
#[derive(Debug, Clone, Default)]
pub struct ModelFields {
    pub description: String,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub model_type: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cost_per_image: f64,
    pub cost_per_audio_second: f64,
    pub cost_per_character: f64,
    pub cost_per_video: f64,
    pub context_window: i64,
    pub admission_weight: i64,
    pub max_in_flight: Option<i64>,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    pub supports_vision: bool,
    pub tags: Vec<String>,
    pub boons: Vec<String>,
    pub tool_servers: Vec<String>,
    pub energy_slots_per_node: i64,
    pub route_bias: f64,
    pub auto_eligible: bool,
    pub draft_model: String,
    pub verify_api_base: String,
    pub verify_upstream_model: String,
    pub aliases: Vec<String>,
    pub quantization: String,
    pub upstream_headers: obleth_config::UpstreamHeaders,
    pub capacity_mode: String,
    pub discovery: obleth_config::capacity::DiscoveryFields,
    pub variants: Vec<obleth_config::ModelVariant>,
}

/// A model to create. Its switch, lifecycle, refused fields, session
/// affinity and health-check interval go into the same insert as its
/// fields, so a failed create leaves no half-configured row behind and the
/// data plane never sees the model before it is set the way it was asked.
#[derive(Debug, Clone)]
pub struct NewModel {
    pub model_name: String,
    pub fields: ModelFields,
    pub enabled: bool,
    pub lifecycle: obleth_config::ModelLifecycle,
    pub rejected_request_fields: Vec<String>,
    pub session_affinity: obleth_config::ModelSessionAffinity,
    pub health_check_interval_secs: i64,
}

impl NewModel {
    /// The schema's default interval between health checks.
    pub const DEFAULT_HEALTH_CHECK_INTERVAL_SECS: i64 = 900;

    /// An enabled, active model with default settings: what a test or an
    /// import needs before it sets the fields it cares about.
    pub fn new(model_name: impl Into<String>, fields: ModelFields) -> Self {
        NewModel {
            model_name: model_name.into(),
            fields,
            enabled: true,
            lifecycle: Default::default(),
            rejected_request_fields: Vec::new(),
            session_affinity: Default::default(),
            health_check_interval_secs: Self::DEFAULT_HEALTH_CHECK_INTERVAL_SECS,
        }
    }
}

/// A rewrite of a model's fields and switch. The refused fields and session
/// affinity are kept as stored when `None`, since they have their own
/// endpoints and a client may leave them out of a full update.
#[derive(Debug, Clone)]
pub struct ModelUpdate {
    pub fields: ModelFields,
    pub enabled: bool,
    pub rejected_request_fields: Option<Vec<String>>,
    pub session_affinity: Option<obleth_config::ModelSessionAffinity>,
}

/// The columns [`ModelFields`] writes, in the order [`bind_model_fields`]
/// binds them. The `discovery` fields take the last five.
const MODEL_FIELD_COLUMNS: [&str; 38] = [
    "description",
    "upstream_model",
    "api_base",
    "api_key",
    "model_type",
    "input_cost_per_token",
    "output_cost_per_token",
    "cost_per_image",
    "cost_per_audio_second",
    "cost_per_character",
    "cost_per_video",
    "context_window",
    "admission_weight",
    "max_in_flight",
    "supports_function_calling",
    "supports_system_messages",
    "supports_response_schema",
    "supports_tool_choice",
    "supports_vision",
    "tags",
    "boons",
    "tool_servers",
    "energy_slots_per_node",
    "route_bias",
    "auto_eligible",
    "draft_model",
    "verify_api_base",
    "verify_upstream_model",
    "aliases",
    "quantization",
    "upstream_headers",
    "capacity_mode",
    "capacity_namespace",
    "capacity_service",
    "per_replica_max_in_flight",
    "capacity_source",
    "capacity_headroom",
    "variants",
];

/// `$from, $from+1, ...` for the [`MODEL_FIELD_COLUMNS`].
fn model_field_placeholders(from: usize) -> String {
    (0..MODEL_FIELD_COLUMNS.len())
        .map(|i| format!("${}", from + i))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `col = $from, col = $from+1, ...` for the [`MODEL_FIELD_COLUMNS`].
fn model_field_assignments(from: usize) -> String {
    MODEL_FIELD_COLUMNS
        .iter()
        .enumerate()
        .map(|(i, col)| format!("{col} = ${}", from + i))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Bind the [`ModelFields`] in [`MODEL_FIELD_COLUMNS`] order, normalized the
/// way every model write normalizes them.
fn bind_model_fields<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    f: &'q ModelFields,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    let discovery = f.discovery.normalized();
    q.bind(&f.description)
        .bind(&f.upstream_model)
        .bind(&f.api_base)
        .bind(cipher().encrypt_opt(f.api_key.as_deref()))
        .bind(obleth_config::normalize_model_type(&f.model_type))
        .bind(f.input_cost_per_token)
        .bind(f.output_cost_per_token)
        .bind(f.cost_per_image.max(0.0))
        .bind(f.cost_per_audio_second.max(0.0))
        .bind(f.cost_per_character.max(0.0))
        .bind(f.cost_per_video.max(0.0))
        .bind(f.context_window.max(0))
        .bind(f.admission_weight.max(1))
        .bind(f.max_in_flight.map(|n| n.max(1)))
        .bind(f.supports_function_calling)
        .bind(f.supports_system_messages)
        .bind(f.supports_response_schema)
        .bind(f.supports_tool_choice)
        .bind(f.supports_vision)
        .bind(sqlx::types::Json(serialize_tag_levels(&f.tags)))
        .bind(sqlx::types::Json(obleth_config::normalize_boons(&f.boons)))
        .bind(sqlx::types::Json(obleth_config::normalize_tool_servers(
            &f.tool_servers,
        )))
        .bind(f.energy_slots_per_node.max(0))
        .bind(f.route_bias)
        .bind(f.auto_eligible)
        .bind(f.draft_model.trim())
        .bind(f.verify_api_base.trim())
        .bind(f.verify_upstream_model.trim())
        .bind(sqlx::types::Json(obleth_config::normalize_aliases(
            &f.aliases,
        )))
        .bind(obleth_config::normalize_quantization(&f.quantization))
        .bind(encrypt_upstream_headers(&f.upstream_headers))
        .bind(obleth_config::normalize_capacity_mode(&f.capacity_mode))
        .bind(discovery.namespace)
        .bind(discovery.service)
        .bind(discovery.per_replica_max_in_flight.map(|n| n.max(1)))
        .bind(discovery.source)
        .bind(discovery.headroom)
        .bind(sqlx::types::Json(obleth_config::normalize_variants(
            &f.variants,
        )))
}

pub(crate) fn encrypt_upstream_headers(
    headers: &obleth_config::UpstreamHeaders,
) -> sqlx::types::Json<obleth_config::UpstreamHeaders> {
    sqlx::types::Json(
        headers
            .iter()
            .map(|(name, value)| (name.clone(), cipher().encrypt(value)))
            .collect(),
    )
}

/// Decrypt the `upstream_headers` column. Tolerant of statements that do not
/// select it (read as none), but a value that fails to decrypt is an error,
/// exactly as it is for `api_key`.
pub(crate) fn upstream_headers_from_row(row: &PgRow) -> Result<obleth_config::UpstreamHeaders> {
    let Ok(stored) =
        row.try_get::<sqlx::types::Json<obleth_config::UpstreamHeaders>, _>("upstream_headers")
    else {
        return Ok(obleth_config::UpstreamHeaders::new());
    };
    stored
        .0
        .into_iter()
        .map(|(name, value)| Ok((name, cipher().decrypt(&value)?)))
        .collect()
}

impl Store {
    pub async fn create_model(&self, model: &NewModel) -> Result<ModelRoute> {
        let sql = format!(
            "insert into models (id, model_name, enabled, lifecycle, rejected_request_fields,
                                 session_affinity, health_check_interval_secs, {cols})
             values ($1, $2, $3, $4, $5, $6, $7, {vals})
             returning {MODEL_COLUMNS}",
            cols = MODEL_FIELD_COLUMNS.join(", "),
            vals = model_field_placeholders(8),
        );
        let q = sqlx::query(&sql)
            .bind(Uuid::new_v4())
            .bind(&model.model_name)
            .bind(model.enabled)
            .bind(sqlx::types::Json(&model.lifecycle))
            .bind(sqlx::types::Json(
                obleth_config::normalize_rejected_request_fields(&model.rejected_request_fields),
            ))
            .bind(sqlx::types::Json(model.session_affinity))
            .bind(model.health_check_interval_secs.max(60));
        let row = bind_model_fields(q, &model.fields)
            .fetch_one(&self.pool)
            .await?;
        model_from_row(&row)
    }

    pub async fn list_models(&self) -> Result<Vec<ModelRoute>> {
        let rows = sqlx::query(&format!(
            "select {MODEL_COLUMNS}
             from models order by model_name"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(model_from_row).collect()
    }

    pub async fn get_model(&self, id: Uuid) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "select {MODEL_COLUMNS}
             from models where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    pub async fn get_model_by_name(&self, model_name: &str) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "select {MODEL_COLUMNS}
             from models where model_name = $1"
        ))
        .bind(model_name)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Which model, if any, already answers to `name` — as its `model_name`,
    /// as one of its aliases, or as one of its variants. Returned as
    /// `(id, model_name)` so a caller validating a write can both skip the row
    /// it is editing and name the conflicting model in its error.
    ///
    /// The three namespaces are deliberately checked together: a client sending
    /// `model: "x"` cannot tell whether `x` is a canonical name, an alias or a
    /// variant, so `x` has to mean exactly one route no matter which column it
    /// lives in.
    pub async fn model_name_owner(&self, name: &str) -> Result<Option<(Uuid, String)>> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        let row = sqlx::query(
            "select id, model_name from models
             where model_name = $1 or aliases @> to_jsonb($1::text)
                or variants @> jsonb_build_array(jsonb_build_object('name', $1::text))
             limit 1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some((r.try_get("id")?, r.try_get("model_name")?))),
            None => Ok(None),
        }
    }

    pub async fn update_model(&self, id: Uuid, update: &ModelUpdate) -> Result<ModelRoute> {
        let sql = format!(
            "update models set enabled = $2,
                rejected_request_fields = coalesce($3, rejected_request_fields),
                session_affinity = coalesce($4, session_affinity),
                {sets}, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}",
            sets = model_field_assignments(5),
        );
        let q = sqlx::query(&sql)
            .bind(id)
            .bind(update.enabled)
            .bind(update.rejected_request_fields.as_deref().map(|fields| {
                sqlx::types::Json(obleth_config::normalize_rejected_request_fields(fields))
            }))
            .bind(update.session_affinity.map(sqlx::types::Json));
        let row = bind_model_fields(q, &update.fields)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    pub async fn delete_model(&self, id: Uuid) -> Result<()> {
        let r = sqlx::query("delete from models where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub async fn update_model_capacity(
        &self,
        id: Uuid,
        max_in_flight: Option<i64>,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set max_in_flight = $2, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(max_in_flight.map(|n| n.max(1)))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Set a model's lifecycle (active, deprecated or retired, with its
    /// replacement and retirement date). The whole object is replaced; the
    /// Management API decides `changed_at` and validates the rest.
    pub async fn set_model_lifecycle(
        &self,
        id: Uuid,
        lifecycle: &obleth_config::ModelLifecycle,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set lifecycle = $2, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(sqlx::types::Json(lifecycle))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Set how a model takes reasoning effort. The whole map is replaced; the
    /// Management API validates it.
    pub async fn set_model_reasoning(
        &self,
        id: Uuid,
        reasoning: &obleth_config::ModelReasoning,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set reasoning = $2, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(sqlx::types::Json(reasoning))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Set whether the discovery endpoints advertise a model.
    pub async fn set_model_listed(&self, id: Uuid, listed: bool) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set listed = $2, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(listed)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Replace the request fields a model refuses. Names are normalized as
    /// [`obleth_config::normalize_rejected_request_fields`] does.
    pub async fn set_model_rejected_request_fields(
        &self,
        id: Uuid,
        fields: &[String],
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set rejected_request_fields = $2, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(sqlx::types::Json(
            obleth_config::normalize_rejected_request_fields(fields),
        ))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Replace a model's session affinity setting. Validated by the caller.
    pub async fn set_model_session_affinity(
        &self,
        id: Uuid,
        affinity: &obleth_config::ModelSessionAffinity,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set session_affinity = $2, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(sqlx::types::Json(affinity))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Every model family, by name.
    pub async fn list_model_families(&self) -> Result<Vec<obleth_config::ModelFamily>> {
        let rows = sqlx::query(
            "select id, name, description, default_level, members, created_at, updated_at
             from model_families order by name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(family_from_row).collect()
    }

    pub async fn get_model_family(&self, id: Uuid) -> Result<obleth_config::ModelFamily> {
        let row = sqlx::query(
            "select id, name, description, default_level, members, created_at, updated_at
             from model_families where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        family_from_row(&row)
    }

    /// The family called `name`, if there is one: `(id, name)`, like
    /// [`Store::model_name_owner`], so a write can skip its own row.
    pub async fn family_name_owner(&self, name: &str) -> Result<Option<(Uuid, String)>> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        let row = sqlx::query("select id, name from model_families where name = $1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| Ok((r.try_get("id")?, r.try_get("name")?)))
            .transpose()
    }

    /// Names of the families that list `model_name` as a member at any level.
    pub async fn families_using(&self, model_name: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "select name from model_families
             where exists (select 1 from jsonb_each_text(members) m where m.value = $1)
             order by name",
        )
        .bind(model_name)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| r.try_get::<String, _>("name").map_err(Into::into))
            .collect()
    }

    pub async fn create_model_family(
        &self,
        name: &str,
        description: &str,
        default_level: obleth_config::ReasoningLevel,
        members: &obleth_config::FamilyMembers,
    ) -> Result<obleth_config::ModelFamily> {
        let row = sqlx::query(
            "insert into model_families (id, name, description, default_level, members)
             values ($1, $2, $3, $4, $5)
             returning id, name, description, default_level, members, created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(description)
        .bind(default_level.as_str())
        .bind(sqlx::types::Json(members))
        .fetch_one(&self.pool)
        .await?;
        family_from_row(&row)
    }

    pub async fn update_model_family(
        &self,
        id: Uuid,
        name: &str,
        description: &str,
        default_level: obleth_config::ReasoningLevel,
        members: &obleth_config::FamilyMembers,
    ) -> Result<obleth_config::ModelFamily> {
        let row = sqlx::query(
            "update model_families
                set name = $2, description = $3, default_level = $4, members = $5,
                    updated_at = now()
              where id = $1
             returning id, name, description, default_level, members, created_at, updated_at",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(default_level.as_str())
        .bind(sqlx::types::Json(members))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        family_from_row(&row)
    }

    pub async fn delete_model_family(&self, id: Uuid) -> Result<()> {
        let done = sqlx::query("delete from model_families where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Set the capacity mode (`static`, `tuned` or `discovered`) for a model.
    /// Switching mode leaves `max_in_flight` and `capacity_tuned_at` untouched
    /// so operators can keep or edit the value the tuner found (and it stays
    /// the fallback of a `discovered` model). `discovery` replaces the
    /// discovery fields when given and leaves them alone when `None`.
    pub async fn update_model_capacity_mode(
        &self,
        id: Uuid,
        capacity_mode: &str,
        discovery: Option<&obleth_config::capacity::DiscoveryFields>,
    ) -> Result<ModelRoute> {
        let discovery = discovery.map(|d| d.normalized());
        let row = sqlx::query(&format!(
            "update models set capacity_mode = $2,
                    capacity_namespace = case when $3 then $4 else capacity_namespace end,
                    capacity_service = case when $3 then $5 else capacity_service end,
                    per_replica_max_in_flight =
                        case when $3 then $6 else per_replica_max_in_flight end,
                    capacity_source = case when $3 then $7 else capacity_source end,
                    capacity_headroom = case when $3 then $8 else capacity_headroom end,
                    updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(obleth_config::normalize_capacity_mode(capacity_mode))
        .bind(discovery.is_some())
        .bind(discovery.as_ref().and_then(|d| d.namespace.clone()))
        .bind(discovery.as_ref().and_then(|d| d.service.clone()))
        .bind(
            discovery
                .as_ref()
                .and_then(|d| d.per_replica_max_in_flight)
                .map(|n| n.max(1)),
        )
        .bind(
            discovery
                .as_ref()
                .map(|d| d.source.clone())
                .unwrap_or_else(|| obleth_config::DEFAULT_CAPACITY_SOURCE.to_string()),
        )
        .bind(discovery.as_ref().map(|d| d.headroom).unwrap_or(1.0))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Apply a value found by auto-tune: set `max_in_flight`, flip the model to
    /// `tuned` mode, and stamp `capacity_tuned_at` so the dashboard can show
    /// when the recommendation was last applied.
    pub async fn apply_tuned_model_capacity(
        &self,
        id: Uuid,
        max_in_flight: i64,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set max_in_flight = $2, capacity_mode = 'tuned',
                    capacity_tuned_at = now(), updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(max_in_flight.max(1))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    pub async fn update_model_admission_weight(
        &self,
        id: Uuid,
        admission_weight: i64,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set admission_weight = $2, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(admission_weight.max(1))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    pub async fn all_resolved_models(&self) -> Result<Vec<(String, ResolvedModel)>> {
        let rows = sqlx::query(
            "select id, model_name, description, upstream_model, api_base, api_key, upstream_headers, model_type, aliases, variants, lifecycle, reasoning, listed, rejected_request_fields, session_affinity, quantization, admission_weight, max_in_flight, enabled,
                    capacity_mode, capacity_source, capacity_namespace, capacity_service,
                    per_replica_max_in_flight, capacity_headroom,
                    cache_enabled, cache_ttl_secs, input_cost_per_token, output_cost_per_token,
                    cost_per_image, cost_per_audio_second, cost_per_character, cost_per_video,
                    context_window, supports_function_calling, supports_system_messages,
                    supports_response_schema, supports_tool_choice, supports_vision, tags, boons, tool_servers,
                    request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode,
                    debug_diagnostics, energy_slots_per_node, route_bias, auto_eligible,
                    draft_model, verify_api_base, verify_upstream_model
             from models where enabled = true",
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        // Fetch every endpoint in one batched query instead of one per model.
        let model_ids: Vec<Uuid> = rows
            .iter()
            .map(|r| r.try_get("id"))
            .collect::<std::result::Result<_, _>>()?;
        let endpoint_rows = sqlx::query(
            "select model_id, id, api_base, api_key, priority, weight, enabled, health_status,
                    max_in_flight
             from model_endpoints
             where model_id = any($1)
             order by priority asc, created_at asc",
        )
        .bind(&model_ids)
        .fetch_all(&self.pool)
        .await?;
        let mut endpoints_by_model: HashMap<Uuid, Vec<ResolvedEndpoint>> = HashMap::new();
        for row in &endpoint_rows {
            endpoints_by_model
                .entry(row.try_get("model_id")?)
                .or_default()
                .push(resolved_endpoint_from_row(row)?);
        }
        // One bulk query over every model's knowledge-collection attachments,
        // rather than one query per model — this runs on every proxy boot and
        // on the 15s registry refresh, so per-model queries would scale with
        // fleet size. See `knowledge::all_model_collection_ids`.
        let mut collections_by_model = self.all_model_collection_ids().await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let name: String = row.try_get("model_name")?;
            let model_id: Uuid = row.try_get("id")?;
            let endpoints = endpoints_by_model.remove(&model_id).unwrap_or_default();
            // Raw tags jsonb may carry `tag:level` strength suffixes; `tags`
            // below strips them to the bare vocabulary the router's overlap
            // match expects, while `declared_levels` keeps the parsed ladder.
            let raw_tags: Vec<String> = row
                .try_get::<sqlx::types::Json<Vec<String>>, _>("tags")
                .map(|j| j.0)
                .unwrap_or_default();
            out.push((
                name.clone(),
                ResolvedModel {
                    model_name: name,
                    aliases: row
                        .try_get::<sqlx::types::Json<Vec<String>>, _>("aliases")
                        .map(|j| j.0)
                        .unwrap_or_default(),
                    variants: variants_from_row(row),
                    lifecycle: lifecycle_from_row(row),
                    reasoning: reasoning_from_row(row),
                    listed: listed_from_row(row),
                    rejected_request_fields: rejected_request_fields_from_row(row),
                    session_affinity: session_affinity_from_row(row),
                    description: row.try_get("description").unwrap_or_default(),
                    upstream_model: row.try_get("upstream_model")?,
                    api_base: row.try_get("api_base")?,
                    api_key: cipher().decrypt_opt(row.try_get("api_key")?)?,
                    upstream_headers: upstream_headers_from_row(row)?,
                    model_type: row.try_get("model_type")?,
                    quantization: row
                        .try_get::<String, _>("quantization")
                        .unwrap_or_else(|_| obleth_config::DEFAULT_QUANTIZATION.to_string()),
                    admission_weight: row.try_get("admission_weight")?,
                    max_in_flight: row
                        .try_get::<Option<i64>, _>("max_in_flight")?
                        .and_then(|n| usize::try_from(n).ok()),
                    capacity_mode: row
                        .try_get::<String, _>("capacity_mode")
                        .unwrap_or_else(|_| obleth_config::DEFAULT_CAPACITY_MODE.to_string()),
                    capacity_source: row
                        .try_get::<String, _>("capacity_source")
                        .unwrap_or_else(|_| obleth_config::DEFAULT_CAPACITY_SOURCE.to_string()),
                    capacity_namespace: row.try_get("capacity_namespace").unwrap_or(None),
                    capacity_service: row.try_get("capacity_service").unwrap_or(None),
                    per_replica_max_in_flight: row
                        .try_get::<Option<i64>, _>("per_replica_max_in_flight")
                        .unwrap_or(None)
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|n| *n > 0),
                    capacity_headroom: row.try_get("capacity_headroom").unwrap_or(1.0),
                    enabled: row.try_get("enabled")?,
                    cache_enabled: row.try_get("cache_enabled")?,
                    cache_ttl_secs: row.try_get("cache_ttl_secs")?,
                    input_cost_per_token: row.try_get("input_cost_per_token")?,
                    output_cost_per_token: row.try_get("output_cost_per_token")?,
                    cost_per_image: row.try_get("cost_per_image")?,
                    cost_per_audio_second: row.try_get("cost_per_audio_second")?,
                    cost_per_character: row.try_get("cost_per_character")?,
                    cost_per_video: row.try_get("cost_per_video").unwrap_or(0.0),
                    context_window: row.try_get("context_window")?,
                    supports_function_calling: row.try_get("supports_function_calling")?,
                    supports_system_messages: row.try_get("supports_system_messages")?,
                    supports_response_schema: row.try_get("supports_response_schema")?,
                    supports_tool_choice: row.try_get("supports_tool_choice")?,
                    supports_vision: row.try_get("supports_vision").unwrap_or(false),
                    tags: obleth_config::normalize_tags(&raw_tags),
                    declared_levels: obleth_config::declared_tag_levels(&raw_tags),
                    boons: row
                        .try_get::<sqlx::types::Json<Vec<String>>, _>("boons")
                        .map(|j| j.0)
                        .unwrap_or_default(),
                    tool_servers: row
                        .try_get::<sqlx::types::Json<Vec<String>>, _>("tool_servers")
                        .map(|j| j.0)
                        .unwrap_or_default(),
                    knowledge_collections: collections_by_model
                        .remove(&model_id)
                        .unwrap_or_default(),
                    request_timeout_secs: row.try_get("request_timeout_secs")?,
                    max_retries: row.try_get("max_retries")?,
                    retry_backoff_ms: row.try_get("retry_backoff_ms")?,
                    endpoint_selection_mode: row.try_get("endpoint_selection_mode")?,
                    debug_diagnostics: row.try_get("debug_diagnostics").unwrap_or(false),
                    // Tolerant read: column added in the energy-accounting migration;
                    // older SQL statements or pre-migration rows degrade to 0 (off).
                    energy_slots_per_node: row.try_get("energy_slots_per_node").unwrap_or(0),
                    // Tolerant read: column added in the route-bias migration;
                    // older SQL statements or pre-migration rows degrade to the
                    // neutral 1.0 rather than a score-zeroing 0.0.
                    route_bias: row.try_get("route_bias").unwrap_or(1.0),
                    // Tolerant read: column added in the auto-eligibility
                    // migration; older SQL statements or pre-migration rows
                    // degrade to eligible, never to a silent exclusion.
                    auto_eligible: row.try_get("auto_eligible").unwrap_or(true),
                    // Tolerant reads: columns added in the speculation-fields
                    // migration; pre-migration rows degrade to "cannot speculate".
                    draft_model: row.try_get("draft_model").unwrap_or_default(),
                    verify_api_base: row.try_get("verify_api_base").unwrap_or_default(),
                    verify_upstream_model: row.try_get("verify_upstream_model").unwrap_or_default(),
                    endpoints,
                },
            ));
        }
        Ok(out)
    }

    /// Build the `auto`-router candidate list from the registered models plus
    /// the latest health/maintenance state. A model is a candidate when
    /// enabled; it is marked unhealthy when its health check reports
    /// `unhealthy` or it is inside a maintenance window. Models without a
    /// health summary are treated as healthy.
    ///
    /// This lives on `Store` rather than in either caller because both the data
    /// plane (boot warm-up and the 15s registry refresh) and the Management
    /// API's routing simulator must produce *the same* candidate set: a
    /// simulator that assembled candidates even slightly differently would
    /// confidently show an operator a decision the gateway would never make.
    pub async fn build_candidates(
        &self,
        tier_source: obleth_config::TierSource,
    ) -> Result<Vec<obleth_config::routing::Candidate>> {
        let now = Utc::now();
        let models = self.all_resolved_models().await?;
        let health = self.list_model_health_summaries().await?;
        let health_by_name: HashMap<String, &obleth_config::ModelHealthSummary> =
            health.iter().map(|h| (h.model_name.clone(), h)).collect();

        let mut candidates: Vec<obleth_config::routing::Candidate> = models
            .into_iter()
            .map(|(name, model)| {
                let healthy = match health_by_name.get(&name) {
                    Some(h) => {
                        h.status != "unhealthy"
                            && h.maintenance_until.map(|m| m <= now).unwrap_or(true)
                    }
                    None => true,
                };
                obleth_config::routing::Candidate {
                    model: std::sync::Arc::new(model),
                    healthy,
                    levels: Vec::new(),
                }
            })
            .collect();
        // Derived here, off the request path, so routing only ever reads the
        // result: deriving cost quantiles per request would be an O(n log n)
        // sort in the hot path.
        obleth_config::routing::derive_levels(&mut candidates, tier_source);
        Ok(candidates)
    }

    /// Turn a model's route on or off, leaving every other field alone.
    pub async fn set_model_enabled(&self, id: Uuid, enabled: bool) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set enabled = $2, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(enabled)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Toggle (and set the TTL of) the response cache for a model.
    pub async fn update_model_cache(
        &self,
        id: Uuid,
        cache_enabled: bool,
        cache_ttl_secs: i64,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set cache_enabled = $2, cache_ttl_secs = $3, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(cache_enabled)
        .bind(cache_ttl_secs.max(0))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Set the per-model reliability controls (upstream timeout, retries,
    /// retry backoff, endpoint-selection mode). A `None` timeout means the
    /// model defers to the global default.
    pub async fn update_model_reliability(
        &self,
        id: Uuid,
        request_timeout_secs: Option<i64>,
        max_retries: i64,
        retry_backoff_ms: i64,
        endpoint_selection_mode: &str,
        debug_diagnostics: bool,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(&format!(
            "update models set request_timeout_secs = $2, max_retries = $3,
                    retry_backoff_ms = $4, endpoint_selection_mode = $5,
                    debug_diagnostics = $6, updated_at = now()
             where id = $1
             returning {MODEL_COLUMNS}"
        ))
        .bind(id)
        .bind(request_timeout_secs.filter(|n| *n >= 1))
        .bind(max_retries.max(0))
        .bind(retry_backoff_ms.max(0))
        .bind(obleth_config::normalize_endpoint_selection_mode(
            endpoint_selection_mode,
        ))
        .bind(debug_diagnostics)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{default_test_fields, serial, FixtureGuard};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn model_upstream_headers_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let headers: obleth_config::UpstreamHeaders = [
            ("x-routing-hint".to_string(), "sticky".to_string()),
            ("x-upstream-token".to_string(), "tok".to_string()),
        ]
        .into();
        let name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(
                &name,
                ModelFields {
                    quantization: "unknown".into(),
                    upstream_headers: headers.clone(),
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        assert_eq!(model.upstream_headers, headers);
        assert_eq!(
            store.get_model(model.id).await.unwrap().upstream_headers,
            headers
        );

        // A narrow write (the capacity toggle) returns the headers too, so the
        // resolver cache it republishes does not lose them.
        let toggled = store
            .update_model_capacity(model.id, Some(8))
            .await
            .expect("capacity");
        assert_eq!(toggled.upstream_headers, headers);
        let resolved = store
            .all_resolved_models()
            .await
            .unwrap()
            .into_iter()
            .find(|(n, _)| n == &model.model_name)
            .expect("model present")
            .1;
        assert_eq!(resolved.upstream_headers, headers);
    }

    /// The lifecycle column round-trips through every read path the admin API
    /// and the data plane use. Needs `OBLETH_TEST_DATABASE_URL`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn model_lifecycle_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(
                &name,
                ModelFields {
                    description: "lifecycle round trip".into(),
                    upstream_model: "upstream-model".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: obleth_config::DEFAULT_MODEL_TYPE.into(),
                    context_window: 8192,
                    admission_weight: 100,
                    supports_system_messages: true,
                    route_bias: 1.0,
                    quantization: "fp8".into(),
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        // A new model is active.
        assert_eq!(model.lifecycle, obleth_config::ModelLifecycle::default());

        let at = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        let lifecycle = obleth_config::ModelLifecycle {
            status: obleth_config::ModelStatus::Deprecated,
            replacement: "gemma4-31b-it".into(),
            retire_at: Some(at("2026-10-19T00:00:00Z")),
            changed_at: Some(at("2026-10-05T16:00:00Z")),
            note: "Ask rc@asu.edu.".into(),
            redirect: true,
        };
        let updated = store
            .set_model_lifecycle(model.id, &lifecycle)
            .await
            .expect("set lifecycle");
        assert_eq!(updated.lifecycle, lifecycle);
        assert_eq!(
            store.get_model(model.id).await.unwrap().lifecycle,
            lifecycle
        );
        assert_eq!(
            store.get_model_by_name(&name).await.unwrap().lifecycle,
            lifecycle
        );
        let resolved = store
            .all_resolved_models()
            .await
            .expect("resolved models")
            .into_iter()
            .find(|(n, _)| n == &name)
            .expect("model present")
            .1;
        assert_eq!(resolved.lifecycle, lifecycle);

        // Back to active.
        let cleared = store
            .set_model_lifecycle(model.id, &obleth_config::ModelLifecycle::default())
            .await
            .expect("clear lifecycle");
        assert_eq!(cleared.lifecycle.status, obleth_config::ModelStatus::Active);
        assert!(cleared.lifecycle.replacement.is_empty());
    }

    /// The reasoning column round-trips through the admin and data-plane
    /// reads, and families can be created, found by name and by member,
    /// updated and deleted. Needs `OBLETH_TEST_DATABASE_URL`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn model_reasoning_and_families_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(
                &name,
                ModelFields {
                    description: "reasoning round trip".into(),
                    upstream_model: "upstream-model".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: obleth_config::DEFAULT_MODEL_TYPE.into(),
                    context_window: 8192,
                    admission_weight: 100,
                    supports_system_messages: true,
                    route_bias: 1.0,
                    quantization: "fp8".into(),
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        // A new model passes effort through.
        assert!(model.reasoning.is_passthrough());

        let reasoning = obleth_config::ModelReasoning {
            mode: obleth_config::ReasoningMode::Mapped,
            default_level: Some(obleth_config::ReasoningLevel::Medium),
            levels: [
                (
                    obleth_config::ReasoningLevel::None,
                    obleth_config::ReasoningAction::Use(obleth_config::ReasoningLevel::Low),
                ),
                (
                    obleth_config::ReasoningLevel::Low,
                    obleth_config::ReasoningAction::Send(
                        serde_json::json!({"reasoning_effort": "low"})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                ),
                (
                    obleth_config::ReasoningLevel::Medium,
                    obleth_config::ReasoningAction::Send(Default::default()),
                ),
                (
                    obleth_config::ReasoningLevel::High,
                    obleth_config::ReasoningAction::Send(
                        serde_json::json!({"reasoning_effort": "max"})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                ),
            ]
            .into_iter()
            .collect(),
        };
        let updated = store
            .set_model_reasoning(model.id, &reasoning)
            .await
            .expect("set reasoning");
        assert_eq!(updated.reasoning, reasoning);
        assert_eq!(
            store.get_model(model.id).await.unwrap().reasoning,
            reasoning
        );
        let resolved = store
            .all_resolved_models()
            .await
            .expect("resolved models")
            .into_iter()
            .find(|(n, _)| n == &name)
            .expect("model present")
            .1;
        assert_eq!(resolved.reasoning, reasoning);
        assert!(resolved.listed, "a new model is listed");

        let hidden = store
            .set_model_listed(model.id, false)
            .await
            .expect("set listed");
        assert!(!hidden.listed);
        assert!(!store.get_model(model.id).await.unwrap().listed);
        let resolved = store
            .all_resolved_models()
            .await
            .expect("resolved models")
            .into_iter()
            .find(|(n, _)| n == &name)
            .expect("model present")
            .1;
        assert!(!resolved.listed);
        assert!(resolved.rejected_request_fields.is_empty());

        let refusing = store
            .set_model_rejected_request_fields(
                model.id,
                &[
                    " bad_words ".to_string(),
                    "bad_words".to_string(),
                    "model".to_string(),
                ],
            )
            .await
            .expect("set rejected request fields");
        assert_eq!(refusing.rejected_request_fields, vec!["bad_words"]);
        assert_eq!(
            store
                .get_model(model.id)
                .await
                .unwrap()
                .rejected_request_fields,
            vec!["bad_words"]
        );
        let resolved = store
            .all_resolved_models()
            .await
            .expect("resolved models")
            .into_iter()
            .find(|(n, _)| n == &name)
            .expect("model present")
            .1;
        assert_eq!(resolved.rejected_request_fields, vec!["bad_words"]);
        assert!(resolved.session_affinity.is_default());

        let own = obleth_config::ModelSessionAffinity {
            enabled: Some(true),
            max_held_percent: Some(50),
            ..Default::default()
        };
        let affine = store
            .set_model_session_affinity(model.id, &own)
            .await
            .expect("set session affinity");
        assert_eq!(affine.session_affinity, own);
        assert_eq!(
            store.get_model(model.id).await.unwrap().session_affinity,
            own
        );
        let resolved = store
            .all_resolved_models()
            .await
            .expect("resolved models")
            .into_iter()
            .find(|(n, _)| n == &name)
            .expect("model present")
            .1;
        assert_eq!(resolved.session_affinity, own);

        let family_name = format!("f-{}", Uuid::new_v4());
        let members = obleth_config::FamilyMembers {
            none: name.clone(),
            low: name.clone(),
            medium: name.clone(),
            high: name.clone(),
        };
        let family = store
            .create_model_family(
                &family_name,
                "one name",
                obleth_config::ReasoningLevel::High,
                &members,
            )
            .await
            .expect("create family");
        let cleanup = family.id;
        assert_eq!(family.default_level, obleth_config::ReasoningLevel::High);
        assert_eq!(family.members, members);
        assert_eq!(
            store.family_name_owner(&family_name).await.unwrap(),
            Some((family.id, family_name.clone()))
        );
        assert_eq!(
            store.families_using(&name).await.unwrap(),
            vec![family_name.clone()]
        );
        assert!(store
            .list_model_families()
            .await
            .unwrap()
            .iter()
            .any(|f| f.id == family.id));

        let renamed = format!("{family_name}-2");
        let changed = store
            .update_model_family(
                family.id,
                &renamed,
                "",
                obleth_config::ReasoningLevel::None,
                &members,
            )
            .await
            .expect("update family");
        assert_eq!(changed.name, renamed);
        assert_eq!(changed.default_level, obleth_config::ReasoningLevel::None);
        assert_eq!(store.family_name_owner(&family_name).await.unwrap(), None);

        store
            .delete_model_family(cleanup)
            .await
            .expect("delete family");
        assert!(matches!(
            store.get_model_family(cleanup).await,
            Err(StoreError::NotFound)
        ));
        assert!(store.families_using(&name).await.unwrap().is_empty());
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` points at a
    /// throwaway Postgres. Skips silently otherwise so unit runs stay hermetic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn model_aliases_and_quantization_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("m-{}", Uuid::new_v4());
        let alias = format!("{name}-mxfp4");
        let variant = format!("{name}-spec");
        let model = store
            .create_model(&NewModel::new(
                &name,
                ModelFields {
                    description: "alias round trip".into(),
                    upstream_model: "upstream-model-mxfp4".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: obleth_config::DEFAULT_MODEL_TYPE.into(),
                    context_window: 8192,
                    admission_weight: 100,
                    supports_system_messages: true,
                    route_bias: 1.0,
                    // A blank entry and a duplicate: normalization drops both.
                    aliases: vec![alias.clone(), String::new(), alias.clone()],
                    // A backend's own spelling, folded onto the vocabulary value.
                    quantization: "FP8-e4m3".into(),
                    // An unknown boon and a blank variant: normalization drops both.
                    variants: vec![
                        obleth_config::ModelVariant {
                            name: format!(" {variant} "),
                            description: "drafted, then verified".into(),
                            boons: vec!["speculation".into(), "teleportation".into()],
                        },
                        obleth_config::ModelVariant::default(),
                    ],
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        assert_eq!(model.aliases, vec![alias.clone()]);
        let expected_variants = vec![obleth_config::ModelVariant {
            name: variant.clone(),
            description: "drafted, then verified".into(),
            boons: vec!["speculation".into()],
        }];
        assert_eq!(model.variants, expected_variants);
        assert_eq!(model.quantization, "fp8");

        // Both columns survive the read paths the admin API and the data plane
        // use, not just the INSERT's own RETURNING clause.
        let fetched = store.get_model(model.id).await.expect("get model");
        assert_eq!(fetched.aliases, vec![alias.clone()]);
        assert_eq!(fetched.quantization, "fp8");
        assert_eq!(fetched.variants, expected_variants);
        let by_name = store.get_model_by_name(&name).await.expect("get by name");
        assert_eq!(by_name.aliases, vec![alias.clone()]);
        assert_eq!(by_name.variants, expected_variants);

        // The hot-path view carries them, so the resolver can publish a key per
        // addressable name and the discovery endpoints can report the format.
        let resolved = store
            .all_resolved_models()
            .await
            .expect("resolved models")
            .into_iter()
            .find(|(n, _)| n == &name)
            .expect("model present")
            .1;
        assert_eq!(resolved.aliases, vec![alias.clone()]);
        assert_eq!(resolved.variants, expected_variants);
        assert_eq!(resolved.quantization, "fp8");
        assert_eq!(
            resolved.addressable_names().collect::<Vec<_>>(),
            vec![name.as_str(), alias.as_str(), variant.as_str()]
        );

        // A name is looked up across both namespaces, because a client sending
        // `model: "x"` cannot tell which column `x` lives in.
        assert_eq!(
            store.model_name_owner(&alias).await.expect("owner"),
            Some((model.id, name.clone()))
        );
        assert_eq!(
            store.model_name_owner(&name).await.expect("owner"),
            Some((model.id, name.clone()))
        );
        assert_eq!(
            store.model_name_owner(&variant).await.expect("owner"),
            Some((model.id, name.clone()))
        );
        assert!(store
            .model_name_owner("no-such-model")
            .await
            .expect("owner")
            .is_none());

        // An update replaces the alias list wholesale: an omitted alias is gone,
        // which is what lets `sync_model_from` know to evict its resolver key.
        let updated = store
            .update_model(
                model.id,
                &ModelUpdate {
                    fields: ModelFields {
                        description: "alias round trip".into(),
                        upstream_model: "upstream-model-mxfp4".into(),
                        api_base: "http://127.0.0.1:8081".into(),
                        model_type: obleth_config::DEFAULT_MODEL_TYPE.into(),
                        context_window: 8192,
                        admission_weight: 100,
                        supports_system_messages: true,
                        route_bias: 1.0,
                        quantization: "mxfp4".into(),
                        ..default_test_fields()
                    },
                    enabled: true,
                    rejected_request_fields: None,
                    session_affinity: None,
                },
            )
            .await
            .expect("update model");
        assert!(updated.aliases.is_empty());
        assert!(updated.variants.is_empty());
        assert_eq!(updated.quantization, "mxfp4");
        assert!(store
            .model_name_owner(&alias)
            .await
            .expect("owner")
            .is_none());
        assert!(store
            .model_name_owner(&variant)
            .await
            .expect("owner")
            .is_none());
    }

    /// The discovered mode's fields survive every read path, the capacity-mode
    /// write replaces them only when given, and an endpoint's own concurrency
    /// reaches the hot-path view.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn capacity_discovery_fields_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        // Re-running the migrations is a no-op, including the constraint swap.
        store.migrate().await.expect("migrate twice");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("m-{}", Uuid::new_v4());
        let discovery = obleth_config::capacity::DiscoveryFields {
            source: " Kubernetes ".into(),
            namespace: Some(" inference ".into()),
            service: Some("served-serve".into()),
            per_replica_max_in_flight: Some(8),
            headroom: 1.25,
        };
        let model = store
            .create_model(&NewModel::new(
                &name,
                ModelFields {
                    description: "discovery round trip".into(),
                    upstream_model: "served".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: obleth_config::DEFAULT_MODEL_TYPE.into(),
                    context_window: 8192,
                    admission_weight: 100,
                    max_in_flight: Some(6),
                    supports_system_messages: true,
                    route_bias: 1.0,
                    capacity_mode: "discovered".into(),
                    discovery: discovery.clone(),
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        let check = |m: &ModelRoute| {
            assert_eq!(m.capacity_mode, "discovered");
            assert_eq!(m.capacity_source, "kubernetes");
            assert_eq!(m.capacity_namespace.as_deref(), Some("inference"));
            assert_eq!(m.capacity_service.as_deref(), Some("served-serve"));
            assert_eq!(m.per_replica_max_in_flight, Some(8));
            assert_eq!(m.capacity_headroom, 1.25);
        };
        check(&model);
        check(&store.get_model(model.id).await.expect("get"));
        check(&store.get_model_by_name(&name).await.expect("by name"));
        check(
            store
                .list_models()
                .await
                .expect("list")
                .iter()
                .find(|m| m.id == model.id)
                .expect("listed"),
        );

        let endpoint = store
            .create_model_endpoint(
                model.id,
                "a",
                "http://127.0.0.1:8082",
                None,
                0,
                100,
                true,
                Some(16),
            )
            .await
            .expect("create endpoint");
        assert_eq!(endpoint.max_in_flight, Some(16));

        let resolved = store
            .all_resolved_models()
            .await
            .expect("resolved")
            .into_iter()
            .find(|(n, _)| n == &name)
            .expect("present")
            .1;
        assert_eq!(resolved.capacity_mode, "discovered");
        assert_eq!(resolved.capacity_source, "kubernetes");
        assert_eq!(resolved.capacity_namespace.as_deref(), Some("inference"));
        assert_eq!(resolved.per_replica_max_in_flight, Some(8));
        assert_eq!(resolved.capacity_headroom, 1.25);
        assert_eq!(resolved.max_in_flight, Some(6), "the static fallback");
        assert_eq!(resolved.endpoints[0].max_in_flight, Some(16));

        // A mode switch with no fields leaves them alone...
        let m = store
            .update_model_capacity_mode(model.id, "static", None)
            .await
            .expect("mode only");
        assert_eq!(m.capacity_mode, "static");
        assert_eq!(m.capacity_service.as_deref(), Some("served-serve"));
        // ...and with fields replaces them.
        let m = store
            .update_model_capacity_mode(
                model.id,
                "discovered",
                Some(&obleth_config::capacity::DiscoveryFields {
                    per_replica_max_in_flight: Some(4),
                    ..Default::default()
                }),
            )
            .await
            .expect("mode and fields");
        assert_eq!(m.capacity_source, "endpoints");
        assert_eq!(m.capacity_namespace, None);
        assert_eq!(m.capacity_service, None);
        assert_eq!(m.per_replica_max_in_flight, Some(4));
        assert_eq!(m.capacity_headroom, 1.0);

        // The range CHECKs hold whatever the caller sends (the vocabularies
        // are checked in obleth-config; see schema/postgres/README.md).
        let bad = sqlx::query("update models set capacity_headroom = 0 where id = $1")
            .bind(model.id)
            .execute(&store.pool)
            .await;
        assert!(bad.is_err(), "headroom must be above 0");

        let updated = store
            .update_model_endpoint(
                model.id,
                endpoint.id,
                "a",
                "http://127.0.0.1:8082",
                None,
                0,
                100,
                true,
                None,
            )
            .await
            .expect("update endpoint");
        assert_eq!(updated.max_in_flight, None);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    ///
    /// Proves the round trip Controller ruling F6 requires: a declared
    /// `tag:level` suffix must survive a save (through the `tags` jsonb
    /// column) and a subsequent read, both via the admin-facing `ModelRoute`
    /// (`get_model`, which reflects exactly what's stored) and via the
    /// data-plane `ResolvedModel` cache view (`all_resolved_models`, which
    /// must see the same suffix decoded into `declared_levels` while `tags`
    /// itself stays bare for the router's overlap match). Also proves the
    /// two behaviors that must NOT change: an unknown tag base is dropped,
    /// and a bare tag stays bare, byte-identical to every row written before
    /// tiering existed. Since `2b86f63` a bare tag also declares *nothing*:
    /// it derives its level from cost rank under `TierSource::Hybrid`, so
    /// only an explicit `:N` suffix reaches `declared_levels`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn declared_tag_level_survives_save_then_read() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let created_tags = vec![
            "coding:3".to_string(),    // declared, tiered
            "math".to_string(),        // declared, untiered -> level 1, stored bare
            "astrology:2".to_string(), // invalid base -> dropped, as today
        ];
        let model = store
            .create_model(&NewModel::new(
                &model_name,
                ModelFields {
                    tags: created_tags.clone(),
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        // The write path re-serializes what survives validation: the invalid
        // `astrology:2` tag is gone, `coding:3` keeps its suffix, and the
        // untiered `math` is bare -- not `math:1`.
        assert_eq!(model.tags, vec!["coding:3".to_string(), "math".to_string()]);

        // Reading the model back (as the admin UI would on open-to-edit)
        // must see the same suffixed form, or a save-without-touching-tags
        // would silently drop the declared level on the next edit.
        let reread = store.get_model(model.id).await.expect("get model");
        assert_eq!(reread.tags, model.tags);

        // The data-plane cache view must decode the suffix into
        // `declared_levels` while keeping `tags` bare, since the router's
        // exact-match overlap logic (`router::select_model`) compares against
        // bare vocabulary strings. Bare `math` declares nothing -- it derives
        // from cost rank instead of silently pinning the model to level 1.
        let resolved = store
            .all_resolved_models()
            .await
            .expect("all resolved models")
            .into_iter()
            .find(|(name, _)| name == &model_name)
            .map(|(_, r)| r)
            .expect("resolved model present");
        assert_eq!(
            resolved.tags,
            vec!["coding".to_string(), "math".to_string()]
        );
        assert_eq!(resolved.declared_levels, vec![("coding".to_string(), 3)]);

        // A save that lowers the declared level to an explicit 1 must KEEP the
        // suffix: bare ("derive my level") and `tag:1` ("pinned weak on
        // purpose") mean different things under hybrid tiering, so the save
        // path must not erase the distinction the operator typed.
        let updated_tags = vec!["coding:1".to_string()];
        let updated = store
            .update_model(
                model.id,
                &ModelUpdate {
                    fields: ModelFields {
                        tags: updated_tags.clone(),
                        ..default_test_fields()
                    },
                    enabled: true,
                    rejected_request_fields: None,
                    session_affinity: None,
                },
            )
            .await
            .expect("update model");
        assert_eq!(updated.tags, vec!["coding:1".to_string()]);
        let reread_after_update = store.get_model(model.id).await.expect("get model");
        assert_eq!(reread_after_update.tags, vec!["coding:1".to_string()]);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// `set_model_enabled` flips only the route switch: the model keeps every
    /// other field, and reads report the new state.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn set_model_enabled_flips_only_the_switch() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model = store
            .create_model(&NewModel::new(
                format!("enabled-{}", Uuid::new_v4()),
                ModelFields {
                    description: "enabled switch test model".into(),
                    upstream_model: "upstream-model".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: "chat".into(),
                    context_window: 8192,
                    admission_weight: 100,
                    max_in_flight: Some(12),
                    supports_system_messages: true,
                    tags: vec!["coding".to_string()],
                    route_bias: 1.0,
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        assert!(model.enabled, "a new model starts enabled");

        let off = store
            .set_model_enabled(model.id, false)
            .await
            .expect("disable");
        assert!(!off.enabled);
        assert_eq!(off.max_in_flight, Some(12));
        assert_eq!(off.tags, vec!["coding".to_string()]);
        assert!(!store.get_model(model.id).await.unwrap().enabled);

        let on = store
            .set_model_enabled(model.id, true)
            .await
            .expect("enable");
        assert!(on.enabled);
        assert!(store.get_model(model.id).await.unwrap().enabled);

        assert!(matches!(
            store.set_model_enabled(Uuid::new_v4(), true).await,
            Err(StoreError::NotFound)
        ));
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Verifies that `debug_diagnostics` round-trips through the store and is
    /// not silently cleared back to `false` by an unrelated update's RETURNING.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn debug_diagnostics_round_trips_and_survives_unrelated_update() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model = store
            .create_model(&NewModel::new(
                format!("diag-{}", Uuid::new_v4()),
                ModelFields {
                    description: "debug diagnostics test model".into(),
                    upstream_model: "upstream-model".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: "chat".into(),
                    context_window: 8192,
                    admission_weight: 100,
                    supports_system_messages: true,
                    route_bias: 1.0,
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        // Enable via the reliability update.
        let saved = store
            .update_model_reliability(model.id, None, 2, 200, "failover", true)
            .await
            .expect("update reliability");
        assert!(
            saved.debug_diagnostics,
            "reliability update should persist the flag"
        );

        // A read SELECT must report it (not the tolerant default).
        assert!(
            store.get_model(model.id).await.unwrap().debug_diagnostics,
            "get_model must return the persisted flag"
        );
        assert!(
            store
                .list_models()
                .await
                .unwrap()
                .iter()
                .find(|m| m.id == model.id)
                .unwrap()
                .debug_diagnostics,
            "list_models must return the persisted flag"
        );

        // An UNRELATED update's RETURNING must not mask it back to false
        // (this is the sync_model cache-poisoning trap).
        store
            .update_model_cache(model.id, true, 60)
            .await
            .expect("update_model_cache (unrelated update)");
        assert!(
            store.get_model(model.id).await.unwrap().debug_diagnostics,
            "unrelated update must not clear debug_diagnostics"
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Verifies that `energy_slots_per_node` round-trips through the store and is
    /// not silently cleared back to `0` by an unrelated update's RETURNING.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn energy_slots_round_trips_and_survives_unrelated_update() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model = store
            .create_model(&NewModel::new(
                format!("eslots-{}", Uuid::new_v4()),
                ModelFields {
                    description: "energy slots test model".into(),
                    upstream_model: "upstream-model".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: "chat".into(),
                    context_window: 8192,
                    admission_weight: 100,
                    supports_system_messages: true,
                    energy_slots_per_node: 8,
                    route_bias: 1.0,
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        assert_eq!(
            model.energy_slots_per_node, 8,
            "create_model must persist energy_slots_per_node"
        );

        // A read SELECT must report the value (not the tolerant default).
        assert_eq!(
            store
                .get_model(model.id)
                .await
                .unwrap()
                .energy_slots_per_node,
            8,
            "get_model must return the persisted value"
        );
        assert_eq!(
            store
                .list_models()
                .await
                .unwrap()
                .iter()
                .find(|m| m.id == model.id)
                .unwrap()
                .energy_slots_per_node,
            8,
            "list_models must return the persisted value"
        );

        // An UNRELATED update's RETURNING must not reset it to 0
        // (same cache-poisoning trap as debug_diagnostics).
        store
            .update_model_cache(model.id, true, 60)
            .await
            .expect("update_model_cache (unrelated update)");
        assert_eq!(
            store
                .get_model(model.id)
                .await
                .unwrap()
                .energy_slots_per_node,
            8,
            "unrelated update must not clear energy_slots_per_node"
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Verifies that `route_bias` round-trips through the store and is not
    /// silently cleared back to the neutral default by an unrelated update's
    /// RETURNING (same cache-poisoning trap as `energy_slots_per_node`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn route_bias_round_trips_and_survives_unrelated_update() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("biased-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(
                &model_name,
                ModelFields {
                    route_bias: 1.4,
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        assert_eq!(
            model.route_bias, 1.4,
            "create_model must persist route_bias"
        );
        assert_eq!(
            store.get_model(model.id).await.unwrap().route_bias,
            1.4,
            "get_model must return the persisted value"
        );

        // An UNRELATED update's RETURNING must not reset it to the default.
        store
            .update_model_cache(model.id, true, 60)
            .await
            .expect("update_model_cache (unrelated update)");
        assert_eq!(
            store.get_model(model.id).await.unwrap().route_bias,
            1.4,
            "unrelated update must not clear route_bias"
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// A create sets everything it was asked in one statement: the switch,
    /// lifecycle, refused fields, session affinity and health interval come
    /// back from the one call, and a create that fails leaves no row. An
    /// update keeps the settings it was not given and clears the ones it was.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_create_sets_every_setting_in_one_statement_or_leaves_nothing() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("m-{}", Uuid::new_v4());
        let affinity = obleth_config::ModelSessionAffinity {
            enabled: Some(true),
            hold_ms: Some(2_000),
            ..Default::default()
        };
        let new = NewModel {
            model_name: name.clone(),
            fields: ModelFields {
                aliases: vec![format!("{name}-alias")],
                quantization: "fp8".into(),
                ..default_test_fields()
            },
            enabled: false,
            lifecycle: obleth_config::ModelLifecycle {
                status: obleth_config::ModelStatus::Staged,
                changed_at: Some(chrono::Utc::now()),
                ..Default::default()
            },
            rejected_request_fields: vec!["logprobs".into(), " logprobs ".into()],
            session_affinity: affinity,
            health_check_interval_secs: 120,
        };
        let model = store.create_model(&new).await.expect("create");
        fixtures.track_model(model.id);
        assert!(!model.enabled);
        assert_eq!(model.lifecycle.status, obleth_config::ModelStatus::Staged);
        assert_eq!(model.rejected_request_fields, vec!["logprobs".to_string()]);
        assert_eq!(model.session_affinity, affinity);
        assert_eq!(model.aliases, vec![format!("{name}-alias")]);
        assert_eq!(model.quantization, "fp8");
        let health = store
            .get_model_health_summary(model.id)
            .await
            .expect("health");
        assert_eq!(health.check_interval_secs, 120);

        // The same name again is refused, and the refused create leaves
        // nothing behind: still one row.
        assert!(store.create_model(&new).await.is_err());
        let rows: i64 = sqlx::query_scalar("select count(*) from models where model_name = $1")
            .bind(&name)
            .fetch_one(store.pool())
            .await
            .expect("count");
        assert_eq!(rows, 1);

        let update = |fields: ModelFields,
                      rejected: Option<Vec<String>>,
                      affinity: Option<obleth_config::ModelSessionAffinity>| {
            ModelUpdate {
                fields,
                enabled: true,
                rejected_request_fields: rejected,
                session_affinity: affinity,
            }
        };
        let kept = store
            .update_model(
                model.id,
                &update(
                    ModelFields {
                        description: "changed".into(),
                        ..default_test_fields()
                    },
                    None,
                    None,
                ),
            )
            .await
            .expect("update");
        assert!(kept.enabled);
        assert_eq!(kept.description, "changed");
        assert_eq!(kept.rejected_request_fields, vec!["logprobs".to_string()]);
        assert_eq!(kept.session_affinity, affinity);
        let cleared = store
            .update_model(
                model.id,
                &update(
                    default_test_fields(),
                    Some(Vec::new()),
                    Some(Default::default()),
                ),
            )
            .await
            .expect("update");
        assert!(cleared.rejected_request_fields.is_empty());
        assert_eq!(cleared.session_affinity, Default::default());
        assert!(matches!(
            store
                .update_model(Uuid::new_v4(), &update(default_test_fields(), None, None))
                .await,
            Err(StoreError::NotFound)
        ));
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// An auto-excluded model must stay excluded across reads, across an
    /// unrelated update, and in the resolved view the router actually consumes.
    /// The tolerant reads that back this column default to `true`, so a
    /// dropped column would silently re-include the model — these assertions
    /// are what distinguish "stored false" from "never read".
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn auto_eligible_round_trips_and_survives_unrelated_update() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("excluded-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(
                &model_name,
                ModelFields {
                    auto_eligible: false,
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        assert!(
            !model.auto_eligible,
            "create_model must persist auto_eligible"
        );
        assert!(
            !store.get_model(model.id).await.unwrap().auto_eligible,
            "get_model must return the persisted value"
        );

        // An UNRELATED update's RETURNING must not silently re-include it.
        store
            .update_model_cache(model.id, true, 60)
            .await
            .expect("update_model_cache (unrelated update)");
        assert!(
            !store.get_model(model.id).await.unwrap().auto_eligible,
            "unrelated update must not clear auto_eligible"
        );

        // The router reads the resolved view, not ModelRoute, so the exclusion
        // has to survive that mapping too.
        let resolved = store
            .all_resolved_models()
            .await
            .expect("resolved models")
            .into_iter()
            .find(|(name, _)| name == &model_name)
            .expect("model in resolved view")
            .1;
        assert!(
            !resolved.auto_eligible,
            "the resolved view the router consumes must carry the exclusion"
        );

        // And the candidate list the router is actually handed must reject it.
        let candidates = store
            .build_candidates(obleth_config::TierSource::Hybrid)
            .await
            .expect("candidates");
        let candidate = candidates
            .iter()
            .find(|c| c.model.model_name == model_name)
            .expect("model in candidate list");
        assert!(!candidate.model.auto_eligible);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// A model created without an explicit choice participates in auto routing:
    /// this migration must not quietly empty an existing deployment's pool.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn auto_eligible_defaults_to_participating() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("included-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        assert!(model.auto_eligible);

        // Pre-migration rows have no value to read at all. Clearing the column
        // default and inserting through it is the closest reproduction of that
        // state, and it must still resolve as eligible.
        sqlx::query("update models set auto_eligible = default where id = $1")
            .bind(model.id)
            .execute(&store.pool)
            .await
            .expect("reset to column default");
        assert!(
            store.get_model(model.id).await.unwrap().auto_eligible,
            "the column default must be eligible"
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Verifies a model created without an explicit bias is neutral (`1.0`),
    /// never the score-zeroing `0.0`, and that the resolved-cache view used
    /// by the router agrees.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn route_bias_defaults_to_neutral() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("plain-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        assert_eq!(
            model.route_bias, 1.0,
            "a model created without a bias must be neutral"
        );

        let resolved = store
            .all_resolved_models()
            .await
            .expect("all_resolved_models")
            .into_iter()
            .find(|(name, _)| name == &model_name)
            .map(|(_, m)| m)
            .expect("resolved model present");
        assert_eq!(
            resolved.route_bias, 1.0,
            "resolved-cache view must also see the neutral default, not 0.0"
        );
    }
}
