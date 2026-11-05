# Explicit task publication and continuation

The native data-plane candidate publishes selected task history independently of
ordinary Git push. A publication can also include explicit ref changes. The
server preserves original record IDs, raw encodings, attachments and completion
proofs; receiving them does not mean the receiving machine executed the commands.

Obtain an instance URL, repository ID, scoped token and trusted receipt public
key from its operator. Set `ALT_SERVER_TOKEN` for the invoking process. The token
does not enter the remote URL, saved binding, publication package or Git export.

```sh
alt task remote connect origin --url https://alt.example.test \
  --repository repository --receipt-key "$RECEIPT_PUBLIC_KEY" --json
```

The saved binding pins the origin, repository instance/epoch, receipt key and
actor/delegation to this local repository. Changed identities require an explicit
new binding. Only literal loopback HTTP is available with
`--allow-loopback-http`. Task remote commands run directly in the invoking
process, so credentials are not taken from a previously started daemon.

A plan file selects roots and optional companion history:

```json
{
  "selection": {
    "roots": [{"kind":"evidence","id":"EVIDENCE_ID"}],
    "companions": [{"kind":"task-baseline","id":"BASELINE_ID"}],
    "omissions": [],
    "attestations": []
  },
  "refs": []
}
```

Use actual lowercase record IDs. `task create --json` returns the task and frozen
baseline IDs. Dependencies are followed automatically; companions are explicit.
Review/Integration/Delivery roots follow their required baseline and history.
Attachments may be explicitly omitted with a recorded reason where permitted;
omission never rewrites an existing record or grants complete acceptance.

```sh
alt task remote preview origin --plan publication.json --json
alt task remote publish PUBLICATION_ID --json
alt task remote status PUBLICATION_ID --json
```

Preview freezes the exact package and returns its inventory and request digest
before any upload. Publish queries that original request, uploads only missing
chunks, assembles and verifies content, then commits. Resume uses the same ID and
intent. A lost response is unknown until queried; expiration, changed epochs or
rejected preconditions do not silently create a replacement request. Successful
receipts are verified against the pinned Ed25519 key and saved locally.

List published roots with `task remote list origin --kind evidence --json`; add
`--id RECORD_ID` to list signed publications containing a record. Use the last
returned record ID or publication request digest as `--after` for the next page.
Receive a specific publication, then explicitly adopt its task in a named native
workspace whose HEAD and integration targets match the original baseline:

```sh
alt task remote receive origin --kind evidence --id EVIDENCE_ID \
  --publication REQUEST_DIGEST --json
alt task remote adopt TASK_ID --publication REQUEST_DIGEST --json
alt candidate create TASK_ID --parent ORIGINAL_CANDIDATE_ID --json
```

Receiving installs verified objects and records, but leaves refs, index and
worktree untouched. Apply the intended changes explicitly before making a new
candidate. Adoption preserves the original task scope and records a new local
binding; new candidate/evidence/review/integration commands have new local request
identities. Complete continuation requires the selected attachments. Imported
commands are never automatically executed. Run validation again with an explicitly
chosen runner profile before accepting the new candidate.

For a task publication that changes refs, include the exact integration target
and notes updates in `refs`, and supply collector attestations accepted by the
repository's current policy. `task remote attest EVIDENCE_ID --key-file KEY --json`
signs the original completed local observed statement; KEY is a private file of
32 raw Ed25519 seed bytes. Signing does not grant current server acceptance. Add
the returned `attestation` to the plan before previewing it.

```sh
alt task remote publish PUBLICATION_ID --prepare-only --json
# With an independently authorized reviewer credential and binding:
alt task remote review reviewer --actor PUBLISHING_ACTOR \
  --request PUBLICATION_ID --json
alt task remote approve reviewer --actor PUBLISHING_ACTOR \
  --request PUBLICATION_ID --integration INTEGRATION_ID --digest REQUEST_DIGEST --json
# With the original publishing actor:
alt task remote publish PUBLICATION_ID --json
```

Review downloads and verifies the complete ready selection, displays its original
review changes within a bounded output budget, and retains all raw content for
inspection. Approval names the inspected digest and rechecks target refs and
policy. The server separately rechecks reviewer authority, trusted collectors,
evidence eligibility and ref preconditions at the final transaction. Original
Review records remain historical facts throughout this process.
