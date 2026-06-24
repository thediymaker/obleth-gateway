-- Idempotent: safe to re-run.

-- The base of the disjoint port window the provisioner assigned this replica.
-- The job binds the first free port in [port_base, port_base + span) and the
-- provisioner discovers the bound port by probing that window.
-- bigint (INT8) to match the Rust i64 the store decodes it into; the
-- alter-type self-heals databases where an earlier revision added it as int.
alter table model_replicas add column if not exists port_base bigint;
alter table model_replicas alter column port_base type bigint;

-- Health floor, distinct from target_replicas (the count the reconciler submits
-- toward). A model is healthy when at least min_replicas replicas are healthy.
-- bigint to match target_replicas (also bigint) and the Rust i64 mapping.
alter table managed_models add column if not exists min_replicas bigint not null default 1;
alter table managed_models alter column min_replicas type bigint;
