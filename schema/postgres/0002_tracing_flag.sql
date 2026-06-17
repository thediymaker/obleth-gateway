-- Idempotent: safe to re-run.

alter table api_keys add column if not exists tracing_enabled boolean not null default false;
alter table tenants  add column if not exists tracing_enabled boolean not null default false;
