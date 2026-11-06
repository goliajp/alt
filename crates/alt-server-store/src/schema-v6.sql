-- Original requests and receipts remain unchanged. This lineage connects their
-- historical authority to the current repository scope after explicit transfer.
CREATE TABLE alt_publication_provenance (
  repository text NOT NULL REFERENCES alt_repositories(id),
  publication_digest text NOT NULL,
  proofs jsonb NOT NULL CHECK (jsonb_typeof(proofs) = 'array' AND jsonb_array_length(proofs) BETWEEN 1 AND 32),
  PRIMARY KEY(repository, publication_digest)
);
