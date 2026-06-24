-- Idempotent: safe to re-run.

-- The base of the disjoint port window the provisioner assigned this replica.
-- The job binds the first free port in [port_base, port_base + span) and the
-- provisioner discovers the bound port by probing that window.
alter table model_replicas add column if not exists port_base int;

-- Health floor, distinct from target_replicas (the count the reconciler submits
-- toward). A model is healthy when at least min_replicas replicas are healthy.
alter table managed_models add column if not exists min_replicas int not null default 1;
