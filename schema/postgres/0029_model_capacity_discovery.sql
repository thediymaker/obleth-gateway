-- Idempotent: safe to re-run.
-- The `discovered` capacity mode: a model's pool size follows its live
-- backend, ready serving replicas x per-replica concurrency (x headroom), read
-- by each gateway replica from a capacity source. max_in_flight stays the
-- fallback used until (or whenever) discovery has no answer.
--   capacity_source            where replicas are counted: 'endpoints' (the
--                              model's own enabled, healthy endpoints) or
--                              'kubernetes' (Ready pods matching a selector)
--   capacity_namespace         kubernetes: namespace of the backend pods; null
--                              searches OBLETH_CAPACITY_DISCOVERY_NAMESPACES
--   capacity_selector          kubernetes: label selector for them; null uses
--                              OBLETH_CAPACITY_DEFAULT_SELECTOR
--   per_replica_max_in_flight  requests one replica takes; null reads a known
--                              concurrency flag off the serving container
--                              (kubernetes) or each endpoint's max_in_flight
--   capacity_headroom          multiplier on the derived value (1 = exactly
--                              the ready capacity)
alter table models add column if not exists capacity_source text not null default 'endpoints';
alter table models add column if not exists capacity_namespace text;
alter table models add column if not exists capacity_selector text;
alter table models add column if not exists per_replica_max_in_flight bigint;
alter table models add column if not exists capacity_headroom double precision not null default 1;

-- Per-endpoint concurrency for the 'endpoints' source, for fleets whose
-- endpoints are not alike. Null uses the model's per_replica_max_in_flight.
alter table model_endpoints add column if not exists max_in_flight bigint;

do $$ begin
    if not exists (
        select 1 from pg_constraint
        where conrelid = 'models'::regclass and conname = 'models_capacity_source_check'
    ) then
        alter table models add constraint models_capacity_source_check
            check (capacity_source in ('endpoints', 'kubernetes'));
    end if;
    if not exists (
        select 1 from pg_constraint
        where conrelid = 'models'::regclass
          and conname = 'models_per_replica_max_in_flight_check'
    ) then
        alter table models add constraint models_per_replica_max_in_flight_check
            check (per_replica_max_in_flight is null or per_replica_max_in_flight >= 1);
    end if;
    if not exists (
        select 1 from pg_constraint
        where conrelid = 'models'::regclass and conname = 'models_capacity_headroom_check'
    ) then
        alter table models add constraint models_capacity_headroom_check
            check (capacity_headroom > 0 and capacity_headroom <= 10);
    end if;
    if not exists (
        select 1 from pg_constraint
        where conrelid = 'model_endpoints'::regclass
          and conname = 'model_endpoints_max_in_flight_check'
    ) then
        alter table model_endpoints add constraint model_endpoints_max_in_flight_check
            check (max_in_flight is null or max_in_flight >= 1);
    end if;
end $$;

-- Widen the capacity_mode vocabulary. Replaced only while it still lacks
-- 'discovered', so a re-run takes no lock on models.
do $$ begin
    if not exists (
        select 1 from pg_constraint
        where conrelid = 'models'::regclass
          and conname = 'models_capacity_mode_check'
          and pg_get_constraintdef(oid) like '%discovered%'
    ) then
        alter table models drop constraint if exists models_capacity_mode_check;
        alter table models add constraint models_capacity_mode_check
            check (capacity_mode in ('static', 'tuned', 'discovered'));
    end if;
end $$;
