-- Idempotent: safe to re-run.

-- Operator-requested replica restart. When true, the provisioner cancels this
-- replica's Slurm job (regardless of target) and the resubmit-to-target launches
-- a fresh one. The flag lives only for the life of the row (a cancelled replica
-- drains, goes lost, and is GC'd), so no explicit clear is needed.
alter table model_replicas add column if not exists cancel_requested boolean not null default false;
