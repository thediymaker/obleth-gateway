-- Idempotent: safe to re-run.

-- Admin-authored recipe templates, created/edited from the dashboard. `body` is
-- the raw recipe document (YAML header + sbatch script) parsed by the control
-- plane — same shape as a *.recipe file, just stored in the DB so it is editable
-- at runtime without redeploying the control plane.
create table if not exists recipes (
    id          uuid primary key,
    name        text not null,
    body        text not null default '',
    author      text not null default '',
    created_at  timestamptz not null default now(),
    updated_at  timestamptz not null default now()
);

create index if not exists recipes_updated_idx on recipes (updated_at desc);

-- Supersedes the orphaned launcher-preset store (its only consumer, the launch
-- wizard, was removed).
drop table if exists saved_recipes;
