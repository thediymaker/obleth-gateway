-- Idempotent: safe to re-run.
-- Batch bytes moved from Postgres to object storage (OBLETH_S3_BUCKET): an
-- item keeps its place in the input file instead of a copy of its request, an
-- answer keeps only its size, and file contents are no longer stored here.
-- Batches stored the old way can neither run nor be read the new way, so they
-- are removed (TRUNCATE: instant, and gives the space back at once).
do $$
begin
    if exists (select 1 from information_schema.columns
               where table_schema = current_schema()
                 and table_name = 'batch_items' and column_name = 'body') then
        drop table if exists batch_file_chunks;
        truncate batch_items, batches, batch_files;
        alter table batch_items
            drop column body,
            drop column response,
            add column input_offset bigint not null,
            add column input_length integer not null,
            add column response_bytes integer;
    end if;
end $$;

alter table batch_files add column if not exists uploading boolean not null default false;
alter table batch_files add column if not exists cleanup_at timestamptz;
alter table batches add column if not exists cleanup_at timestamptz;
