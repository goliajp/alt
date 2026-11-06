# GOLIA platform contract

Version 0.1.0. Shared public wire types for account authorization, product installation audiences, PKCE exchanges and bounded service leases. The Cloud provider owns accounts and sessions; product runtimes consume this contract without accessing its database. Parsing is not authorization. Consumers must validate exact audiences, trusted-clock expiry and persisted sequence/revocation before each use.

Canonical source is `core/crates/platform-contract` in the GOLIA platform source tree. Public consumers vendor a versioned source package with its file digests and both licenses; no private source path is needed to build.

`lifecycle` defines versioned, immutable product deployment specifications and challenged readiness responses. Application/database images are pinned by digest; Alt also pins its private-listener edge image. The digest covers the exact audience, region, images and independent runtime/data/database secrets. A readiness response is useful only after authentication and exact audience/spec/challenge comparison; callers still perform independent network probes and fence completion by the current job lease and runtime generation. No resource deletion or automatic generation change is implied.

Navigation snapshots bind the current account lease and the exact shared workspace. Products appear only with current content access; consumers validate identity and freshness, then expose the public view without upstream account/session claims. A destination starts a new product login and rechecks access.
