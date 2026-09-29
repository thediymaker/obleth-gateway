-- Idempotent: safe to re-run.
-- Launch history for Slurm-provisioned replicas. model_replicas rows are
-- garbage-collected (lost rows after retention, draining rows once their job
-- is gone), so they are no record of past launches. One row here per replica
-- (same id), written by the store as the replica moves through its states,
-- and never deleted with it. No foreign keys on purpose: the history must
-- outlive the replica, the managed spec and the model.
create table if not exists deployment_launches (
    id              uuid primary key,          -- = model_replicas.id
    model_id        uuid not null,
    model_name      text not null,
    recipe_id       text,                      -- launcher_spec->>'recipe_id'
    slurm_job_id    text not null,
    -- Snapshot of the managed spec at submit time.
    partition       text,
    account         text,
    qos             text,
    time_limit      text,
    gres            text,
    nodes_requested bigint,
    cpus_per_task   bigint,
    mem             text,
    exclude         text,
    constraints     text,
    launcher_spec   jsonb,
    nodes           text,                      -- allocated nodes
    submitted_at    timestamptz not null default now(),
    started_at      timestamptz,
    healthy_at      timestamptz,
    ended_at        timestamptz,
    -- Slurm's terminal state (TIMEOUT, OUT_OF_MEMORY, NODE_FAIL, FAILED,
    -- COMPLETED, CANCELLED, PREEMPTED, ...), `gone` when the job vanished
    -- from slurmrestd, or `cancelled:scale-down|restart|probe-failed` when
    -- obleth cancelled it.
    end_state       text,
    end_reason      text,
    updated_at      timestamptz not null default now()
);

create index if not exists deployment_launches_model_submitted_idx
    on deployment_launches (model_id, submitted_at desc);
create index if not exists deployment_launches_recipe_submitted_idx
    on deployment_launches (recipe_id, submitted_at desc);
