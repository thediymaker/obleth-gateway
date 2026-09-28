-- Idempotent: safe to re-run.
-- Which tenant owns each video generation job, and where it lives.
--
-- The OpenAI Videos API is job-based: `POST /v1/videos` returns an id, and the
-- polls (`GET /v1/videos/{id}`), the download (`GET /v1/videos/{id}/content`)
-- and the delete carry that id and nothing else. The gateway records the id at
-- create time so a follow-up can be routed to the model that made it and
-- refused (as not found) for any other tenant.
--
--   model_name     the registered model the job was created through
--   tenant_id      owner; every lookup is scoped by it
--   key_id         the API key that created the job (audit only; any key of
--                  the tenant may follow it up)
--   upstream_base  the base URL that accepted the create. A model with several
--                  endpoints (clusters that share no storage) must be asked
--                  about the job on the one that holds it.
--
-- Postgres rather than Redis on purpose: the chart's Redis evicts keys that
-- carry a TTL under memory pressure, and losing a row mid-render strands the
-- job. Rows are pruned by age (created_at) by the gateway itself; the backend
-- forgets finished jobs long before that.
create table if not exists video_jobs (
    job_id        text primary key,
    model_name    text not null,
    tenant_id     uuid not null,
    key_id        uuid not null,
    upstream_base text not null,
    created_at    timestamptz not null default now()
);

create index if not exists video_jobs_tenant_created_idx on video_jobs (tenant_id, created_at desc);
create index if not exists video_jobs_created_idx on video_jobs (created_at);
