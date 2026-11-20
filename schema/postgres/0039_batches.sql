-- Idempotent: safe to re-run.
-- Batches: the OpenAI Files and Batches APIs.
--
-- A caller uploads a JSONL file of requests (`batch_files`, purpose `batch`),
-- then creates a batch from it (`batches`). Each line becomes a row in
-- `batch_items`, which the gateway's batch runner claims with a lease, sends
-- through the normal pipeline at background priority, and finishes with the
-- upstream's answer. Output and error files are rows in `batch_files` too
-- (purpose `batch_output`, `batch_error`) with no content of their own: they
-- are read from the batch's items when downloaded.
--
-- Only state lives here. The bytes (input files, answers) are in the object
-- store (OBLETH_S3_BUCKET), under batches/: a file as chunk objects, an item as its
-- place in its input file, an answer as its size. The gateway deletes an input
-- file when its batch ends, and a batch with its answers once it has been
-- over for OBLETH_BATCH_RETENTION; objects first, then the rows naming them.

create table if not exists batch_files (
    id          text primary key,
    tenant_id   uuid not null,
    key_id      uuid not null,
    purpose     text not null,
    filename    text not null,
    bytes       bigint not null default 0,
    batch_id    text,
    created_at  timestamptz not null default now(),
    -- An upload still arriving: hidden from every read. `bytes` counts what
    -- is stored so far, so an abandoned upload's objects can be found.
    uploading   boolean not null default false,
    -- When a replica took the file to delete; another may retry it later.
    cleanup_at  timestamptz
);

create index if not exists batch_files_owner_idx
    on batch_files (tenant_id, key_id, created_at desc);
create index if not exists batch_files_created_idx on batch_files (created_at);

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
    -- When a request of the batch last finished: a batch in progress with
    -- requests left and no progress for a while is waiting for capacity.
    last_progress_at  timestamptz,
    metadata          jsonb,
    errors            jsonb,
    created_at        timestamptz not null default now(),
    in_progress_at    timestamptz,
    finalizing_at     timestamptz,
    completed_at      timestamptz,
    failed_at         timestamptz,
    expired_at        timestamptz,
    cancelling_at     timestamptz,
    cancelled_at      timestamptz,
    -- When a replica took the batch to delete; another may retry it later.
    cleanup_at        timestamptz
);

create index if not exists batches_owner_idx on batches (tenant_id, key_id, created_at desc);
create index if not exists batches_active_idx on batches (status)
    where status in ('in_progress', 'cancelling');
create index if not exists batches_created_idx on batches (created_at);
-- The admin list, newest first, across every tenant.
create index if not exists batches_recent_idx on batches (created_at desc, id desc);

create table if not exists batch_items (
    batch_id         text not null references batches (id) on delete cascade,
    line             integer not null,
    custom_id        text not null,
    -- The request's line in the batch's input file: byte offset and length.
    input_offset     bigint not null,
    input_length     integer not null,
    -- pending, running, succeeded, failed, cancelled, expired
    status           text not null default 'pending',
    attempts         integer not null default 0,
    next_attempt_at  timestamptz not null default now(),
    lease_owner      text,
    lease_until      timestamptz,
    response_status  integer,
    -- Size of the stored answer, when there is one; the object's key holds
    -- the attempt that wrote it (see the gateway's batches module).
    response_bytes   integer,
    error            jsonb,
    request_id       uuid,
    finished_at      timestamptz,
    primary key (batch_id, line)
);

create index if not exists batch_items_pending_idx
    on batch_items (batch_id, next_attempt_at, line) where status = 'pending';
create index if not exists batch_items_running_idx
    on batch_items (lease_until) where status = 'running';
