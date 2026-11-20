-- Runs once (see README.md); also safe to re-run.

-- Each document's stored size (chunk text characters plus embedding bytes of
-- its active generation), written when a generation commits, so collection
-- sizes and the chunk cap are read from document rows instead of summing the
-- chunk table on every collection read.
alter table knowledge_documents
    add column if not exists chunk_bytes bigint not null default 0;

-- Documents committed before this column existed.
update knowledge_documents d
   set chunk_bytes = s.bytes
  from (select k.document_id, sum(length(k.text) + octet_length(k.embedding)) as bytes
          from knowledge_chunks k
          join knowledge_documents kd
            on kd.id = k.document_id and k.generation = kd.active_generation
         group by k.document_id) s
 where d.id = s.document_id
   and d.chunk_bytes = 0;
