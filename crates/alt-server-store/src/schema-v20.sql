-- A committed request holds its publication once, in its own column: the stored
-- result of the request and the outbox event of the publication leave it out, and
-- reads put it back from that row. The migration step that goes with this version
-- rewrites the rows older versions wrote. The publication documents are compressed
-- with lz4 from here on; a value stored before keeps the compression it has.
ALTER TABLE alt_requests ALTER COLUMN publication SET COMPRESSION lz4, ALTER COLUMN result SET COMPRESSION lz4;
ALTER TABLE alt_audit ALTER COLUMN event SET COMPRESSION lz4;
ALTER TABLE alt_outbox ALTER COLUMN event SET COMPRESSION lz4;
