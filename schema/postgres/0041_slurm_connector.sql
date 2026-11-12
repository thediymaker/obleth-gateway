-- Idempotent: safe to re-run.

-- Managed deployments: startup deadline, walltime handoff, and the pause and
-- backoff state the provisioner keeps after failed launches or rejected
-- submits.
alter table managed_models
    add column if not exists startup_timeout_mins bigint not null default 60,
    add column if not exists handoff_mins bigint not null default 15,
    add column if not exists paused_reason text,
    add column if not exists paused_until timestamptz,
    add column if not exists failed_launches bigint not null default 0,
    add column if not exists backoff_step bigint not null default 0;

-- target_replicas = 0 keeps the spec but runs no replicas.
alter table managed_models drop constraint if exists managed_models_target_replicas_check;
alter table managed_models drop constraint if exists managed_models_target_replicas_nonneg;
alter table managed_models
    add constraint managed_models_target_replicas_nonneg check (target_replicas >= 0);

-- Replicas: surge-first replacement, the Slurm times the replica was promoted
-- under, a plain-sentence detail for the dashboard, where the job's output
-- goes, and the port the server was found on.
alter table model_replicas
    add column if not exists replace_requested boolean not null default false,
    add column if not exists job_started_at timestamptz,
    add column if not exists job_ends_at timestamptz,
    add column if not exists state_detail text,
    add column if not exists log_path text,
    add column if not exists port bigint;

-- Launch history keeps environment variable names, not their values.
update deployment_launches
   set launcher_spec = jsonb_set(
           launcher_spec, '{env}',
           (select coalesce(jsonb_object_agg(k, '"(hidden)"'::jsonb), '{}'::jsonb)
              from jsonb_object_keys(launcher_spec->'env') as k))
 where launcher_spec is not null
   and jsonb_typeof(launcher_spec->'env') = 'object'
   and exists (select 1 from jsonb_each(launcher_spec->'env') e
                where e.value <> '"(hidden)"'::jsonb);
