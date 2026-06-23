-- Idempotent: safe to re-run.

-- Stores the launcher form state (backend id, knob values, envelope) as JSON so the
-- dashboard "edit" view can faithfully restore the panel. Metadata only — the
-- provisioner ignores it; the rendered launch_command/script_body remain authoritative.
alter table managed_models add column if not exists launcher_spec jsonb;
