-- Idempotent: safe to re-run.

-- Admin-authored, shared launch presets for the Slurm model launcher. Unlike the
-- read-only YAML "curated examples" on disk, these are created/edited from the
-- dashboard and shared across all admins of the instance. `spec` is the launcher
-- form payload (backend id, knob values, default envelope fields) as JSON so new
-- fields need no migration.
create table if not exists saved_recipes (
    id          uuid primary key,
    name        text not null,
    backend     text not null default '',   -- recipe/backend id, e.g. 'llamacpp'
    author      text not null default '',    -- display label of who saved it
    spec        jsonb not null default '{}'::jsonb,
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

create index if not exists saved_recipes_updated_idx on saved_recipes (updated_at desc);
