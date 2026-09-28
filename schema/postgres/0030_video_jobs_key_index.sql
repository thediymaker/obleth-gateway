-- Idempotent: safe to re-run.
-- Video jobs are private to the API key that created them by default
-- (OBLETH_VIDEO_JOB_SCOPE=key): a poll, download or delete matches the job id,
-- the tenant and the caller's key_id, and GET /v1/videos lists only the
-- caller's own jobs. key_id was recorded from the start (0028), so no data
-- changes; this index serves the per-key listing, newest first, without
-- walking the whole tenant's rows. Lookups by id stay on the primary key.
-- OBLETH_VIDEO_JOB_SCOPE=tenant keeps using video_jobs_tenant_created_idx.
create index if not exists video_jobs_tenant_key_created_idx
    on video_jobs (tenant_id, key_id, created_at desc);
