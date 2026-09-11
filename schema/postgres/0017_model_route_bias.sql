-- Idempotent: safe to re-run.
-- Per-model multiplier applied to the `auto` router's final score. 1.0 is
-- neutral; above 1.0 prefers the model, below 1.0 de-prioritizes it.
alter table models add column if not exists route_bias double precision not null default 1.0;
