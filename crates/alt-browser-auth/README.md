# alt-browser-auth

Pure browser authentication primitives for the alt server: canonical public origins, host-only cookies, exact Origin/CSRF checks, duplicate-cookie rejection, random session secrets and bounded Argon2id password verification. This crate performs no network or database I/O.

Session ownership, expiration, rate admission, revocation and repository authorization belong to `alt-server-store`. An accepted password or cookie encoding never grants repository access by itself. Local accounts are a self-hosted adapter; Cloud identity does not fall back to them.
