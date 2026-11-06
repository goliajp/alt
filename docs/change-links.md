# Delivery associations with Sentori

A connection associates an already published Alt Delivery with one exact Sentori
application, release ID, release name, environment and artifact SHA-256. Both
products verify their own current permissions and identities. A signed receipt
records the peer's decision at that time; it does not grant access or establish
that the mapping remains current.

## Pair the instances

An operator provisions both endpoints, verification keys, credentials and allowed
network destinations. Exchange verification keys through an authenticated
operator channel. Alt uses its instance receipt key; Sentori uses the private
Ed25519 seed configured by its change-link runtime. Keep credential and seed files
private. The receiving product independently authenticates its own credential.

Add a `change_links` array to the Alt instance configuration. For example:

```json
{
  "change_links": [{
    "scope": {
      "installation_id": "ALT_INSTALLATION_ID",
      "repository_instance_id": "REPOSITORY_ID",
      "recovery_epoch": "1"
    },
    "channel": "sentori",
    "application": "SENTORI_APPLICATION_ID",
    "peer": {
      "product": "sentori",
      "installation": "SENTORI_INSTALLATION_ID",
      "workspace": "SENTORI_WORKSPACE_ID",
      "scope": "SENTORI_PROJECT_ID",
      "generation": "1"
    },
    "peer_public_key": "SENTORI_64_CHARACTER_HEX_VERIFICATION_KEY",
    "target": {
      "origin": "https://sentori.example.org",
      "policy": {"kind": "public_https"}
    },
    "peer_browser_origin": "https://sentori.example.org",
    "local_credential_file": "/private/alt-current-credential.json",
    "peer_credential_file": "/private/sentori-api.token"
  }]
}
```

Merge this member into the complete server configuration. `recovery_epoch` and
peer `generation` are decimal **strings**. Use the actual repository epoch and
instance generation. Credential paths must be absolute. `local_credential_file`
accepts Alt's private login file or registered token; the peer credential file
contains the Sentori API token. Credentials remain subject to expiry, revocation
and Cloud authorization. A public HTTPS destination rejects private DNS answers,
redirects and ambient proxy routing. An explicitly pinned private destination is
an operator configuration, never an input from an event or repository.

Configure Sentori's reciprocal connection with the same channel and application,
its own local endpoint and current API credential, Alt's endpoint/key/credential,
and a bounded history budget. In self-hosted Sentori the workspace identity is
its instance ID; Cloud uses the platform workspace ID. `peer_browser_origin` is
optional and separately controls browser links. A missing browser address does
not prevent protocol exchange; user-supplied event fields never choose it.

Install each pairing explicitly, then restart the configured services:

```sh
alt-server --config server.json --credential admin.json \
  link-install REPOSITORY_ID EPOCH sentori
# With Sentori's configured database and SENTORI_CHANGE_LINK_CONFIG:
sentori-server change-link-install SENTORI_PROJECT_ID sentori
```

Alt's installing administrator still needs repository publication authority.
Changing the configuration does not implicitly install a new pairing. Disable
Alt's pairing with `link-disable REPOSITORY_ID EPOCH sentori`; signed historical
facts remain readable under current access. Restarting cannot resurrect a
persistently disabled pairing. Restoration and migration preserve history, but
do not carry current credentials, active pairings or pending authorization to a
new identity or epoch.

## Associate a published Delivery

Open a Delivery in the Alt workbench and choose **Delivery associations**. Enter
an existing Sentori release ID/name, its exact environment and artifact digest.
Save and inspect the request before sending. Sentori checks the application,
release and retained artifact; the transaction records the native/Git commit and
original Alt publication provenance from the actual Delivery.

The CLI exposes the same operation. A mapping plan is:

```json
{
  "channel": "sentori",
  "action": {
    "kind": "mapping",
    "delivery_id": "DELIVERY_ID",
    "release": {
      "application": "SENTORI_APPLICATION_ID",
      "release": "SENTORI_RELEASE_ID",
      "release_name": "app@1.0.0",
      "environment": "production",
      "artifact_sha256": "ARTIFACT_SHA256"
    },
    "expected_previous": null
  }
}
```

```sh
alt task remote changes channels origin --json
alt task remote changes preview origin --plan mapping.json --json
alt task remote changes send CHANGE_ID --json
alt task remote changes status CHANGE_ID --json
alt task remote changes list origin --json
alt task remote changes show origin EVENT_DIGEST --json
```

Preview saves an immutable local request. If a response is lost, query that
request before resending it. The browser likewise saves requests in its local
journal; retain that browser profile until an unconfirmed outcome is resolved.
Peer downtime leaves synchronization pending and does not roll back the committed
repository transaction. After rotating an expired/revoked credential, the same
actor can explicitly authorize a pending event with
`changes reauthorize origin sentori EVENT_DIGEST --json`.

For a replacement mapping, name its previous event digest. Withdrawal is an
explicit `action` with `kind: "withdraw"`, `mapping_event_digest`,
`expected_previous` and `reason`. It preserves the original statement. Never
interpret an old applied receipt as a current mapping after withdrawal.

## Request and execute a repair

Sentori's selected occurrence shows only its current exact release/environment
mapping. An unmapped version does not reuse another version's Delivery. Following
a link requires independent authorization in Alt.

A Sentori user can save and send a repair request for the selected occurrence and
an explicit target ref. Only occurrence identities and version metadata are
transferred; telemetry payloads are not copied into a Task. An Alt user then
inspects the signed request and supplies a normal Task definition:

```sh
alt task remote changes repair origin REPAIR_DIGEST --json
alt task remote changes repair-create origin REPAIR_DIGEST \
  --definition 'TASK_SPEC_JSON' --target refs/heads/main --json
```

Omit `repair_source` from the definition: the command verifies and supplies it.
Creation returns ordinary Task and baseline IDs. Publish them through the native
task workflow. The server atomically binds the received repair to its first
published Task and queues the notification to Sentori. Other clients observe that
same Task instead of claiming a second one.

A creation interrupted before local completion requires inspecting the original
request and explicitly reconciling it with `--reconcile-request REQUEST_ID` under
a new global `--request-id`. Original request bytes remain unchanged. A Task
binding does not prove execution: continue through candidate, observed evidence,
review, integration and Delivery publication, including current server approval.

## Observe the resulting release

Upload the repaired release's actual artifact to Sentori and associate the new
Delivery explicitly. A runtime observation names a receipt-time interval and its
exact mapping. Counts cover persisted telemetry only; sampling coverage is
unknown. Zero events does not establish a successful repair. Original requests,
Task bindings, verified execution evidence, Delivery mappings and observations
remain distinct facts in both products.
