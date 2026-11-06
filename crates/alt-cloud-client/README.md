# alt-cloud-client

Bounded server-to-server GOLIA platform client and sealed grant storage. It consumes the versioned public contract vendored with alt; it has no dependency on a private Cloud database or source tree.

The client uses one configured control origin, refuses redirects and browser-origin authentication, bounds responses and validates exact product installation audiences and lease expiry. The caller persists sequence and revocation comparisons in its database transaction before using a claim. A response alone never authorizes repository access.

Opaque platform grants are encrypted with XChaCha20-Poly1305 using a key derived for this purpose from the private runtime credential. Associated data binds the exact audience and local credential digest. Runtime key rotation invalidates old encrypted grants and requires fresh authentication. Neither a grant nor a runtime credential is logged or exposed through `Debug`.
