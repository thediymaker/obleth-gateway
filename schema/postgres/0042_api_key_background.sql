-- Idempotent: safe to re-run.
-- A background key's requests run in each model's background fairshare pool,
-- the one batch items use: they take only spare capacity and never hold back
-- interactive requests. Operator-only, so off by default.
alter table api_keys add column if not exists background boolean not null default false;
