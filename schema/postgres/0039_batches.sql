-- Idempotent: safe to re-run.
-- Batches: the OpenAI Files and Batches APIs.
--
-- A caller uploads a JSONL file of requests (`batch_files`, purpose `batch`;
-- its bytes in `batch_file_chunks`, written as the upload streams in),
-- then creates a batch from it (`batches`). Each line becomes a row in
-- `batch_items`, which the gateway's batch runner claims with a lease, sends
-- through the normal pipeline at background priority, and finishes with the
-- upstream's answer. Output and error files are rows in `batch_files` too
-- (purpose `batch_output`, `batch_error`) with no content of their own: their
-- content is read from `batch_items` when downloaded.
--
-- Postgres rather than Redis for the same reason as the job records: these
-- rows must outlive any pod and must not be evicted. The gateway prunes them
-- by age.

create table if not exists batch_files (
    id          text primary key,
    tenant_id   uuid not null,
    key_id      uuid not null,
    purpose     text not null,
    filename    text not null,
    bytes       bigint not null default 0,
    batch_id    text,
    created_at  timestamptz not null default now()
);

create index if not exists batch_files_owner_idx
    on batch_files (tenant_id, key_id, created_at desc);
create index if not exists batch_files_created_idx on batch_files (created_at);

-- An uploaded file's bytes, in order, a few MB per row, so neither an upload
-- nor a read ever holds the whole file in memory.
create table if not exists batch_file_chunks (
    file_id  text not null references batch_files (id) on delete cascade,
    seq      integer not null,
    data     bytea not null,
    primary key (file_id, seq)
);

create table if not exists batches (
    id                text primary key,
    tenant_id         uuid not null,
    -- The creating key: the runner resolves it again before its requests,
    -- so a disabled key or inactive tenant stops the batch.
    key_id            uuid not null,
    model             text not null,
    endpoint          text not null,
    input_file_id     text not null,
    output_file_id    text,
    error_file_id     text,
    completion_window text not null,
    deadline_at       timestamptz not null,
    status            text not null,
    total             integer not null default 0,
    completed         integer not null default 0,
    failed            integer not null default 0,
    metadata          jsonb,
    errors            jsonb,
    created_at        timestamptz not null default now(),
    in_progress_at    timestamptz,
    finalizing_at     timestamptz,
    completed_at      timestamptz,
    failed_at         timestamptz,
    expired_at        timestamptz,
    cancelling_at     timestamptz,
    cancelled_at      timestamptz
);

create index if not exists batches_owner_idx on batches (tenant_id, key_id, created_at desc);
create index if not exists batches_active_idx on batches (status)
    where status in ('in_progress', 'cancelling');
create index if not exists batches_created_idx on batches (created_at);

create table if not exists batch_items (
    batch_id         text not null references batches (id) on delete cascade,
    line             integer not null,
    custom_id        text not null,
    body             text not null,
    -- pending, running, succeeded, failed, cancelled, expired
    status           text not null default 'pending',
    attempts         integer not null default 0,
    next_attempt_at  timestamptz not null default now(),
    lease_owner      text,
    lease_until      timestamptz,
    response_status  integer,
    response         bytea,
    error            jsonb,
    request_id       uuid,
    finished_at      timestamptz,
    primary key (batch_id, line)
);

create index if not exists batch_items_pending_idx
    on batch_items (batch_id, next_attempt_at, line) where status = 'pending';
create index if not exists batch_items_running_idx
    on batch_items (lease_until) where status = 'running';
