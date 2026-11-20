-- Runs once (see README.md); also safe to re-run.

-- Indexes nothing needs:
-- - model_replicas_model_idx and model_endpoints_model_idx are prefixes of the
--   unique indexes on (model_id, slurm_job_id) and (model_id, name).
-- - model_replicas_state_idx and tenants_status_idx: no query filters on those
--   columns alone.
-- - batches_created_idx: batches_recent_idx (created_at desc, id desc) serves
--   the same scans in either direction.
-- - models_enabled_idx, models_health_due_idx and mcp_servers_enabled_idx index
--   tables of tens of rows, which a sequential scan reads faster. Without
--   models_health_due_idx, a health check's update of health_next_check_at
--   can be a HOT update instead of rewriting every index on models.
drop index if exists model_replicas_model_idx;
drop index if exists model_endpoints_model_idx;
drop index if exists model_replicas_state_idx;
drop index if exists tenants_status_idx;
drop index if exists batches_created_idx;
drop index if exists models_enabled_idx;
drop index if exists models_health_due_idx;
drop index if exists mcp_servers_enabled_idx;

-- Left unread since 0023 moved speculation onto the target model.
alter table models drop column if exists verifier_for;

-- Vocabularies are checked in obleth-config only (README.md): a CHECK here
-- had to be rewritten for every new value, and one left stale (0011) failed
-- saves in production. Numeric-range CHECKs stay.
alter table models drop constraint if exists models_capacity_source_check;
alter table models drop constraint if exists models_capacity_mode_check;
alter table models drop constraint if exists models_endpoint_selection_mode_check;
alter table model_replicas drop constraint if exists model_replicas_state_check;
alter table knowledge_documents drop constraint if exists knowledge_documents_status_check;
