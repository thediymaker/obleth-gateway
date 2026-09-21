-- Idempotent: safe to re-run.
-- Per-key fairshare: weight splits a tenant's share among its keys; max_in_flight
-- is a per-model in-flight ceiling for the key.
alter table api_keys add column if not exists weight bigint not null default 100;
alter table api_keys add column if not exists max_in_flight bigint;
do $$
begin
    if not exists (select 1 from pg_constraint where conname = 'api_keys_weight_check') then
        alter table api_keys add constraint api_keys_weight_check check (weight >= 1);
    end if;
end $$;
