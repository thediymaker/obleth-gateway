-- Institutional knowledge base: collections of uploaded documents, chunked and
-- embedded so the gateway can ground chat requests on them.
-- Idempotent: safe to re-run on every boot.

create table if not exists knowledge_collections (
    id                       uuid primary key,
    name                     text not null unique,
    description              text not null default '',
    -- the embedder the operator *wants*; may differ from what the active
    -- generation was built with, which is what surfaces "needs re-index".
    embedding_model          text not null,
    indexed_embedding_model  text not null default '',
    embedding_dim            integer not null default 0,
    chunk_tokens             integer not null default 400 check (chunk_tokens > 0),
    chunk_overlap_tokens     integer not null default 50 check (chunk_overlap_tokens >= 0),
    -- bumped on any content change; the proxy compares this to decide whether
    -- to rebuild its in-memory slab.
    version                  bigint not null default 0,
    created_at               timestamptz not null default now(),
    updated_at               timestamptz not null default now()
);

-- chunk_overlap_tokens must stay below chunk_tokens: an overlap at or above the
-- target makes the chunker accumulate content instead of advancing, producing
-- chunks that grow without bound. Added via a guarded block (rather than inline
-- in `create table`) so this file remains re-runnable on a database that already
-- has the table.
do $$
begin
    if not exists (
        select 1 from pg_constraint
         where conname = 'knowledge_collections_overlap_lt_tokens'
    ) then
        alter table knowledge_collections
            add constraint knowledge_collections_overlap_lt_tokens
            check (chunk_overlap_tokens < chunk_tokens);
    end if;
end $$;

create table if not exists knowledge_documents (
    id                       uuid primary key,
    collection_id            uuid not null references knowledge_collections(id) on delete cascade,
    title                    text not null,
    filename                 text not null default '',
    content_type             text not null default 'text/plain',
    content                  text not null,
    content_hash             text not null,
    byte_size                bigint not null default 0,
    status                   text not null default 'pending'
                               check (status in ('pending','indexing','ready','failed')),
    error                    text,
    chunk_count              integer not null default 0,
    -- generation is scoped to the DOCUMENT, not the collection: each document
    -- advances its own generation as it is (re)indexed, independently of every
    -- other document, so indexing one never invalidates another's chunks.
    active_generation        integer not null default 0,
    indexed_embedding_model  text not null default '',
    created_at               timestamptz not null default now(),
    indexed_at               timestamptz
);

-- Idempotent guards so this file stays re-runnable against a database that
-- already created the table in its earlier (pre-correction) shape.
alter table knowledge_documents add column if not exists active_generation integer not null default 0;
alter table knowledge_documents add column if not exists indexed_embedding_model text not null default '';

-- Stamped when a document is claimed for indexing, so a document stranded by a
-- crash or deploy can be reclaimed on a timeout. Status alone cannot express
-- this: a claimed row is no longer `pending`, so nothing would ever retry it.
alter table knowledge_documents add column if not exists indexing_started_at timestamptz;

create unique index if not exists knowledge_documents_hash_uq
    on knowledge_documents (collection_id, content_hash);
create index if not exists knowledge_documents_status_idx
    on knowledge_documents (status);

create table if not exists knowledge_chunks (
    id            uuid primary key,
    document_id   uuid not null references knowledge_documents(id) on delete cascade,
    collection_id uuid not null references knowledge_collections(id) on delete cascade,
    generation    integer not null default 0,
    ordinal       integer not null,
    text          text not null,
    token_count   integer not null default 0,
    -- little-endian f32, length = embedding_dim * 4
    embedding     bytea not null,
    embedding_dim integer not null
);

create index if not exists knowledge_chunks_slab_idx
    on knowledge_chunks (collection_id, generation);

-- A 768-dim f32 vector is ~3KB, over the ~2KB TOAST threshold, so Postgres
-- would attempt pglz compression on every write and read. Normalized float
-- vectors are effectively incompressible, so that work is pure waste.
alter table knowledge_chunks alter column embedding set storage external;

create table if not exists model_knowledge_collections (
    model_id      uuid not null references models(id) on delete cascade,
    collection_id uuid not null references knowledge_collections(id) on delete cascade,
    primary key (model_id, collection_id)
);
