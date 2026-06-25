-- Idempotent: safe to re-run.

-- The base of the disjoint port window the provisioner assigned this replica.
-- The job binds the first free port in [port_base, port_base + span) and the
-- provisioner discovers the bound port by probing that window.
-- bigint (INT8) to match the Rust i64 the store decodes it into; the
-- alter-type self-heals databases where an earlier revision added it as int.
alter table model_replicas add column if not exists port_base bigint;
alter table model_replicas alter column port_base type bigint;

-- Backfill replicas that predate this column: a NULL port_base decodes to 0 in
-- the provisioner, which would make it probe ports 0..span and never rediscover
-- a running replica. Seed it from the model's configured serving_port (the
-- window base the provisioner would now assign). Idempotent: only touches NULLs.
update model_replicas r
   set port_base = m.serving_port
  from managed_models m
 where r.model_id = m.model_id
   and r.port_base is null;
-- Any replica with no managed_models row (orphan) still falls back to its
-- model's serving port via the join above; remaining NULLs (none expected) stay
-- NULL and are handled by the decode fallback.

-- Health floor, distinct from target_replicas (the count the reconciler submits
-- toward). A model is healthy when at least min_replicas replicas are healthy.
-- bigint to match target_replicas (also bigint) and the Rust i64 mapping.
alter table managed_models add column if not exists min_replicas bigint not null default 1;
alter table managed_models alter column min_replicas type bigint;
