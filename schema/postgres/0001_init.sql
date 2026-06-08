-- obleth config source-of-truth + audit schema.
-- Idempotent: applied on gateway boot and safe to re-run.

-- Fairshare groups: capacity is partitioned by group weight, then split
-- evenly among tenants within each group (hierarchical algorithm).
create table if not exists fairshare_groups (
    name        text primary key,
    weight      bigint not null default 100 check (weight >= 1),
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

insert into fairshare_groups (name, weight)
values ('default', 100)
on conflict (name) do nothing;

create table if not exists tenants (
    id                uuid primary key,
    name              text not null unique,
    -- fairshare weight; higher = larger share under contention (priority boost)
    weight            bigint not null default 100 check (weight >= 1),
    -- sustained token budget refilled per minute (token-bucket rate)
    tokens_per_minute bigint not null default 60000 check (tokens_per_minute >= 0),
    -- optional per-tenant in-flight cap; null = only the global limit applies
    max_in_flight     bigint,
    fairshare_group   text not null default 'default' references fairshare_groups (name),
    -- free-text operator note plus optional grouping/contact metadata
    description       text not null default '',
    organization      text not null default '',
    contact_email     text not null default '',
    -- lifecycle: active (normal), suspended (temporarily blocked), archived
    -- (retired but preserved). Only 'active' tenants admit traffic.
    status            text not null default 'active',
    -- scheduling: IANA timezone the windows/cutoffs are evaluated in, an
    -- optional activation start and expiry cutoff, and optional recurring
    -- weekly windows (jsonb array of {day:0-6 (0=Sunday), start_min, end_min}).
    -- null active_from/active_until = no bound; null/empty weekly_windows = any time.
    timezone          text not null default 'UTC',
    active_from        timestamptz,
    active_until       timestamptz,
    weekly_windows     jsonb,
    -- term budget cap: an optional cumulative token and/or USD-cost ceiling over
    -- a rolling period (lifetime | monthly | term). budget_started_at marks when
    -- the current term began; changing it (or a monthly roll) resets usage.
    budget_tokens      bigint,
    budget_cost_usd    double precision,
    budget_period      text,
    budget_started_at  timestamptz,
    -- optional per-tenant model allowlist (jsonb array of model_name strings).
    -- null/empty = every registered model is permitted.
    allowed_models     jsonb,
    created_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now()
);

create index if not exists tenants_fairshare_group_idx on tenants (fairshare_group);

-- Idempotent column adds for databases provisioned before these fields existed.
alter table tenants add column if not exists description text not null default '';
alter table tenants add column if not exists organization text not null default '';
alter table tenants add column if not exists contact_email text not null default '';
alter table tenants add column if not exists status text not null default 'active';
alter table tenants add column if not exists timezone text not null default 'UTC';
alter table tenants add column if not exists active_from timestamptz;
alter table tenants add column if not exists active_until timestamptz;
alter table tenants add column if not exists weekly_windows jsonb;
alter table tenants add column if not exists budget_tokens bigint;
alter table tenants add column if not exists budget_cost_usd double precision;
alter table tenants add column if not exists budget_period text;
alter table tenants add column if not exists budget_started_at timestamptz;
alter table tenants add column if not exists allowed_models jsonb;

create index if not exists tenants_status_idx on tenants (status) where status <> 'active';

create table if not exists api_keys (
    id          uuid primary key,
    tenant_id   uuid not null references tenants (id) on delete cascade,
    name        text not null,
    -- display-only prefix for dashboards, e.g. sk_a1b2c3
    key_prefix  text not null,
    -- sha-256 hex of the raw secret; the secret itself is never stored
    key_hash    text not null unique,
    disabled    boolean not null default false,
    created_at  timestamptz not null default now()
);

create index if not exists api_keys_tenant_id_idx on api_keys (tenant_id);

create table if not exists audit_log (
    id          bigserial primary key,
    ts          timestamptz not null default now(),
    actor       text not null,
    action      text not null,
    entity_type text not null,
    entity_id   text not null,
    detail      jsonb not null default '{}'::jsonb
);

create index if not exists audit_log_ts_idx on audit_log (ts desc);

-- Model registry: client-facing names mapped to upstream OpenAI-compatible endpoints.
-- obleth routes by model_name; Aibrix/vLLM or any compatible API handles inference.
create table if not exists models (
    id                          uuid primary key,
    model_name                  text not null unique,
    description                 text not null default '',
    upstream_model              text not null,
    api_base                    text not null,
    api_key                     text,
    -- modality of the model, which determines the OpenAI endpoint it serves:
    -- 'chat' (default), 'embedding', 'audio_transcription', 'audio_speech', 'image'.
    model_type                  text not null default 'chat',
    input_cost_per_token        double precision not null default 0,
    output_cost_per_token       double precision not null default 0,
    -- per-unit costs for non-chat modalities (USD). image: per generated image;
    -- audio_transcription: per second of audio; audio_speech: per input character.
    cost_per_image              double precision not null default 0,
    cost_per_audio_second       double precision not null default 0,
    cost_per_character          double precision not null default 0,
    context_window              bigint not null default 8192 check (context_window >= 0),
    admission_weight            bigint not null default 100 check (admission_weight >= 1),
    -- optional per-model in-flight cap; null = no model-specific cap, the
    -- global fairshare scheduler capacity remains the outer limit
    max_in_flight               bigint check (max_in_flight is null or max_in_flight >= 1),
    -- how max_in_flight is decided: 'static' = operator-set (default; cloud
    -- models pair this with a cap to bound spend); 'tuned' = found by the
    -- auto-tune ramp probe against the upstream (local/self-hosted models).
    capacity_mode               text not null default 'static' check (capacity_mode in ('static', 'tuned')),
    -- when the tuned value was last written by auto-tune (null until tuned).
    capacity_tuned_at           timestamptz,
    supports_function_calling   boolean not null default false,
    supports_system_messages    boolean not null default true,
    supports_response_schema    boolean not null default false,
    supports_tool_choice        boolean not null default false,
    enabled                     boolean not null default true,
    -- exact-match response caching; opt-in with operator-controlled TTL
    cache_enabled               boolean not null default false,
    cache_ttl_secs              bigint not null default 300 check (cache_ttl_secs >= 0),
    -- health probing through obleth's own proxy path, with per-model
    -- scheduling and alert controls for maintenance windows
    health_checks_enabled       boolean not null default true,
    health_alerts_enabled       boolean not null default true,
    health_check_interval_secs  bigint not null default 900 check (health_check_interval_secs >= 60),
    health_failure_threshold    bigint not null default 2 check (health_failure_threshold >= 1),
    health_maintenance_until    timestamptz,
    health_maintenance_note     text,
    health_status               text not null default 'unknown',
    health_consecutive_failures bigint not null default 0 check (health_consecutive_failures >= 0),
    health_alert_state          text not null default 'ok',
    health_next_check_at        timestamptz not null default now(),
    health_last_checked_at      timestamptz,
    health_last_latency_ms      bigint,
    health_last_http_status     bigint,
    health_last_message         text,
    created_at                  timestamptz not null default now(),
    updated_at                  timestamptz not null default now()
);

create index if not exists models_enabled_idx on models (enabled) where enabled = true;

-- Routing tags for the `auto` router (fixed vocabulary, stored as a JSON array
-- so adding tags needs no migration). Added via if-not-exists for upgrades.
alter table models add column if not exists tags jsonb not null default '[]'::jsonb;

-- Capacity tuning mode (added via if-not-exists for upgrades). 'static' keeps
-- the operator-set max_in_flight; 'tuned' lets auto-tune set it from a ramp
-- probe against the upstream.
alter table models add column if not exists capacity_mode text not null default 'static';
alter table models add column if not exists capacity_tuned_at timestamptz;
do $$ begin
    if not exists (
        select 1 from information_schema.constraint_column_usage
        where table_name = 'models' and constraint_name = 'models_capacity_mode_check'
    ) then
        alter table models add constraint models_capacity_mode_check
            check (capacity_mode in ('static', 'tuned'));
    end if;
end $$;

create index if not exists models_health_due_idx
    on models (health_next_check_at)
    where enabled = true and health_checks_enabled = true;

create table if not exists model_health_checks (
    id               bigserial primary key,
    model_id         uuid not null references models(id) on delete cascade,
    checked_at       timestamptz not null default now(),
    trigger          text not null,
    status           text not null,
    latency_ms       bigint,
    http_status      bigint,
    message          text,
    response_excerpt text
);

create index if not exists model_health_checks_model_ts_idx
    on model_health_checks (model_id, checked_at desc);

create index if not exists model_health_checks_ts_idx
    on model_health_checks (checked_at desc);

-- MCP (Model Context Protocol) server registry. obleth reverse-proxies these
-- through its auth + audit layer so clients reach many MCP servers via one
-- authenticated endpoint (/mcp/{name}).
create table if not exists mcp_servers (
    id           uuid primary key,
    name         text not null unique,
    upstream_url text not null,
    auth_header  text,
    enabled      boolean not null default true,
    created_at   timestamptz not null default now(),
    updated_at   timestamptz not null default now()
);

create index if not exists mcp_servers_enabled_idx on mcp_servers (enabled) where enabled = true;

-- Runtime-editable application settings (alerting config, etc.). Single row per
-- key, value held as JSON so new fields don't require migrations.
create table if not exists app_settings (
    key        text primary key,
    value      jsonb not null,
    updated_at timestamptz not null default now()
);
