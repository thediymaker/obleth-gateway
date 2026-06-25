-- Idempotent: safe to re-run.
-- The provisioner's last submit failure for a managed model, surfaced in the
-- dashboard (e.g. Slurm "error 2045 — invalid account/partition combination").
-- Cleared on a successful submit.
alter table managed_models add column if not exists last_provision_error text;
alter table managed_models add column if not exists last_provision_error_at timestamptz;
