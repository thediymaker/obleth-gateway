-- Idempotent: safe to re-run.

-- Per-managed-model JSON metadata. Recipe deploys store {source:"recipe", recipe_id,
-- engine, name} here so the edit view can identify recipe-sourced rows. Metadata
-- only — the provisioner ignores it.
alter table managed_models add column if not exists launcher_spec jsonb;
