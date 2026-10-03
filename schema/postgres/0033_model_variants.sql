-- Idempotent: safe to re-run.
-- Opt-in names for a model with extra boons on, e.g. `glm-5-3-spec` = the
-- same route as `glm-5-3` (same backend, capacity pool and prices) with the
-- speculation boon added. A JSON array of
-- {"name": ..., "description": ..., "boons": [...]}, so adding one needs no
-- migration. Every pre-migration row reads as "no variants".
alter table models add column if not exists variants jsonb not null default '[]'::jsonb;

-- Variant names share one namespace with model names and aliases, checked by
-- containment (`variants @> '[{"name": "x"}]'`) when a name is written, which
-- jsonb_path_ops indexes.
create index if not exists models_variants_idx on models using gin (variants jsonb_path_ops);
