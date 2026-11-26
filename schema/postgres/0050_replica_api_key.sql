-- Runs once (see README.md); also safe to re-run.

-- A Slurm deployment can give each replica its own API key, so the model
-- server answers only requests that carry it (the gateway and the
-- provisioner) and not anything else that can reach its port on the cluster
-- network. Off unless the deployment turns it on.
alter table managed_models
    add column if not exists replica_api_key boolean not null default false;

-- The key a replica's job was started with, encrypted like the other stored
-- secrets. Null for a replica started without one.
alter table model_replicas
    add column if not exists api_key text;
