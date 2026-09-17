-- Idempotent: safe to re-run.
-- Separates a model's identity from how it happens to be deployed.
--
--   aliases       extra client-facing names that resolve to the same route
--                 (JSON array, so adding one needs no migration). This is what
--                 makes a name cleanup non-breaking: drop `-fp8` from
--                 model_name, list the old spelling here, and pinned clients
--                 keep working while only model_name is advertised by the
--                 discovery endpoints.
--   quantization  the weight/activation format this deployment serves, from
--                 the fixed QUANTIZATIONS vocabulary. Descriptive only — it
--                 never affects routing. 'unknown' (the default, and what
--                 every pre-migration row reads as) means nobody declared it;
--                 'none' is the deliberate statement that weights are full
--                 precision.
alter table models add column if not exists aliases jsonb not null default '[]'::jsonb;
alter table models add column if not exists quantization text not null default 'unknown';

-- Alias lookups are exact-match containment (`aliases @> to_jsonb('name')`),
-- used to find which model already answers to a name so a colliding write is
-- rejected. jsonb_path_ops indexes `@>` but not `?`, which is why the query
-- side is written with containment.
create index if not exists models_aliases_idx on models using gin (aliases jsonb_path_ops);
