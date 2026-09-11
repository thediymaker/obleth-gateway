-- Idempotent: safe to re-run.
-- Whether the `auto` router may select this model. False removes the model from
-- auto's candidate pool while leaving it fully addressable by name, which is
-- what distinguishes this from `enabled = false`. Defaults to true so existing
-- deployments keep their current auto behaviour across this migration.
alter table models add column if not exists auto_eligible boolean not null default true;
