# Changelog

## 0.1.0

First self-hosted release of the alt server.

- Git smart HTTP (push, clone, fetch) and the native task protocol on one
  repository store, with the browser workbench.
- Git objects are stored as verified immutable pack segments: a push is
  checked once and published with its refs in one database transaction;
  clones resend the stored bytes.
- Offline bundles for linux/amd64 and linux/arm64: application, PostgreSQL 18
  and an HTTPS edge under Docker Compose, with explicit initialization,
  encrypted backup, verification and restore.
