-- A ref is written only by a transaction that holds the lock of its repository's
-- row, and repositories are never removed, so the foreign key guarded nothing while
-- it was checked once for every ref a push created.
ALTER TABLE alt_refs DROP CONSTRAINT alt_refs_repository_fkey;
