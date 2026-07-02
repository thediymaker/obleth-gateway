-- Idempotent: safe to re-run.

alter table models add column if not exists energy_slots_per_node bigint not null default 0;
