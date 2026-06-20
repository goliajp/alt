# Anti-rot · atom layer + xref protocol

**Status:** design · phase 0 (architecture only · no implementation yet)
**Authoring window:** 2026-06-19
**Maintenance gates:** see `.claude/CLAUDE.md` · design doc gates 1 (audit) + 2 (redundancy) apply to this file too.

---

## Why this exists

`docs/design/mental-model.html` proved the problem the hard way: 87 scenarios cross-referenced as free-text `"见场景 17"`, command names hand-quoted from the CLI enum, 全 86 场景 written as a literal across 7 final-summary blocks. Every owner-strategic change (添加场景 84/85/86) triggered manual gate-1 audit fixes in 5+ places. Reference rot is the dominant failure mode of growing design docs and growing codebases. git/jj/sapling solve **state** drift (signed op / op log / immutable history) but leave **reference** drift to wiki discipline.

Anti-rot is the *immune system* for invariants 1–7 — without it, those invariants degrade as the doc/repo evolves.

This protocol is the *meta-layer*. It is not invariant 8 (it doesn't lock state). It locks **reference structure**.

---

## Position in alt's principle stack

```
invariant 1  signed op            — attribution can't drift
invariant 2  op log               — history can't drift
invariant 3  portable patch       — transport can't drift
invariant 4  workspace isolation  — concurrency can't drift
invariant 5  byte-exact git compat — bytes can't drift
invariant 6  immutable history    — commits can't drift
invariant 7  LLM isolation (cand.) — architecture can't drift
─────────────────────────────────────────────────────────────
meta · anti-rot xref integrity    — REFERENCES can't drift
                                    (the immune system for all above)
```

It is also the **fourth candidate axis** of `alt ⊇ git`:

| axis | what alt knows beyond git |
|---|---|
| 1 · format-native (§ 6) | what each file *is* and how to diff it |
| 2 · visibility (scenario 84) | which paths are public vs alt-only |
| 3 · structured commit msg (scenario 85) | message has schema, not free text |
| **4 · reference integrity** | typed pointers between content with self-verify |

---

## Atom layer — the SSOT primitive

### Definition

An **atom** is `alt`'s universal addressable source-of-truth unit. Every typed concept (scenario, invariant, axis, command, prism, file, commit, issue) is an atom.

### Identity (three layers · all required)

| layer | example | role | persistence |
|---|---|---|---|
| **semantic id** | `amend` | URL slot · human-readable · IDE autocompletes | mutable via rename protocol; never reused |
| **uuid** | `01931a8f-7c9f-7000-...` (UUID v7) | internal stable backing · survives rename | immutable from creation |
| **content hash** | `sha256:a1b2c3...` | verification · pins an exact version | one per version; history retained |

### Auxiliary fields

- **aliases**: `["scenario-17", "amend-last-commit"]` — old semantic ids · permanently kept · URL resolver falls back through them
- **ordering**: `17` — purely visual sequence · changes don't touch refs (refs use semantic id, not number)
- **history**: append-only audit (跟 invariant 6 immutable history 同性质) — every rename / status change / content edit appends a signed entry
- **out_refs**: explicit declaration of outgoing edges — `alt-xref` derives backlinks from these
- **content_hash**: derived sha256 over canonical-JSON of user-facing fields (excludes uuid/aliases/history/ordering/content_hash itself)

---

## URL scheme

### Grammar

```
url       := "alt://" kind "/" atom-id [ "@" pin ] [ sub-path ]

kind      := lowercase identifier (a-z, 0-9, dash)
atom-id   := semantic-kebab   | uuid-form   | external-form
              | path-form (for path kind only)

pin       := "hash:" hex      | "ordering:" int
              | "uuid:" uuid    (forces resolution by uuid not semantic)

sub-path  := ("/" segment)*   — atom kind decides the sub-grammar
```

### Examples

```
alt://scenario/amend                     — top-level atom
alt://scenario/amend/step/3              — step 3 inside the atom
alt://scenario/amend/silent/2            — silent block 2
alt://scenario/amend@hash:abcd...        — pinned to specific content version
alt://scenario/amend@ordering:17         — view hint (renderer can show number)

alt://invariant/signed-op
alt://invariant/llm-isolation@cand       — candidate status

alt://cmd/commit
alt://cmd/flow.feature.start             — dotted sub-command path

alt://prism/markdown
alt://axis/2                             — visibility
alt://principle/anti-rot                 — this very protocol is itself an atom

alt://path/src/parser.rs                 — display path; backing is internal uuid
alt://path/src/parser.rs@uuid:01...      — force uuid resolution (survives rename)

alt://commit/a1b2c3...                   — alt repo oid
alt://op/_op_1842                        — op log entry

alt://issue/github/123
alt://issue/linear/ALT-456
alt://pr/github/789
alt://ci/github-actions/12345
```

### Resolution algorithm

```
1. parse url → (kind, atom-id, pin?, sub-path?)
2. load SoT for kind
3. lookup atom by atom-id:
     a. exact match on .id
     b. fallback: scan .aliases (each atom)
     c. fallback: parse atom-id as uuid form, lookup by .uuid
4. if pin present:
     - hash:X  → load that specific historical version
     - ordering:N → no-op (view hint only)
     - uuid:U  → assert match
5. if sub-path: descend via atom-kind's sub-resolver
6. emit: { atom, hash, display, target_url, status }
7. errors:
     - kind unknown                                → BROKEN_REF
     - id resolves to nothing (after aliases+uuid) → BROKEN_REF
     - resolves to deprecated atom                  → STALE_REF (warn)
     - sub-path invalid for atom kind               → BROKEN_REF
     - hash pin doesn't match any historical hash   → BROKEN_REF
```

---

## SoT files

### Location

```
docs/design/sources/
├── scenarios.yaml      — alt://scenario/*
├── invariants.yaml     — alt://invariant/*
├── axes.yaml           — alt://axis/* (alt ⊇ git axes)
├── dimensions.yaml     — alt://dimension/* (D1–D4)
├── principles.yaml     — alt://principle/* (anti-rot, llm-isolation, three-axes meta)
├── prisms.yaml         — alt://prism/* (derived from crates/alt-prism-*)
├── concepts.yaml       — alt://concept/* (workspace, op-log, preset, chunk-store, …)
├── sections.yaml       — alt://section/* (mental-model.html § ids)
└── commands.yaml       — alt://cmd/* (generated · do not edit · from alt-cli enum)
```

### Per-kind ownership rule

An atom is **declared once**, in its home SoT file. Every other file may only *reference* it. Verifier rejects:
- same atom id declared in two SoT files
- two atoms with the same uuid (cross-file)
- alias collision (same alias on two different atoms)

### Generated SoTs

- `commands.yaml` — emitted by `alt atom rebuild cmd` parsing `crates/alt-cli/src/cli.rs` Command enum
- `prisms.yaml` — emitted from `crates/alt-prism-*` crate metadata
- both checked into git so reviewers see drift in diffs

Manual edit of generated SoT → verifier error (header `# generated · do not edit`).

### Schema example (scenarios.yaml entry)

```yaml
schema_version: 1
kind: scenario
atoms:
  - id: amend
    uuid: 01931a8f-7c9f-7000-8000-000000000017
    aliases:
      - scenario-17
      - amend-last-commit
    ordering: 17
    section: alt://section/cli-workflow      # ref, not "§ 5" string
    title: "amend · 改最近一个 commit"
    status: notimpl                          # ✅ done | 🟡 partial | ❌ notimpl | 🔮 future
    priority: m18                            # m18 | m19 | m20 | m21-plus | unscheduled
    summary: |
      刚 commit 完发现 message 写错或漏文件。改最近这一次。
    out_refs:
      - alt://invariant/op-log               # uses
      - alt://cmd/undo                       # workaround uses
      - alt://scenario/social-pr             # affects
      - alt://scenario/rebase-i              # blocks
    content_hash: sha256:auto-derived        # written by alt atom verify --emit-hash
    history:
      - at: 2026-06-19T13:24:00Z
        by: owner
        action: created
        hash: sha256:initial...
```

---

## Rename protocol

```
alt atom rename alt://scenario/amend alt://scenario/fix-last-commit
```

Effect:
1. yaml entry `.id` flips: `amend` → `fix-last-commit`
2. previous id automatically prepended to `.aliases`: `["amend", ...prior...]`
3. `.history` appends:
   ```yaml
   - at: 2026-06-19T15:00:00Z
     by: owner
     action: rename
     from: amend
     to: fix-last-commit
   ```
4. `.uuid` does NOT change
5. `.content_hash` does NOT change (id is excluded from hash content)
6. all `alt://scenario/amend` refs in existing docs resolve via alias fallback — **no doc rewrite required**

The rename is one signed op. Roll back via `alt undo` (跟 invariant 2 一体).

Semantic ids are **never reused** for a different atom — verifier rejects creating new atom with id equal to any alias anywhere.

---

## Deprecation protocol

```
alt atom deprecate alt://scenario/foo --reason "merged into bar"
```

Effect:
1. `.status` set to `deprecated`
2. `.history` appended with reason + signed op
3. atom remains resolvable forever (跟 invariant 6 一体)
4. xref verifier emits STALE_REF (yellow) for any incoming refs, with hint to migrate

Atoms are never deleted from yaml. Tombstone forever.

---

## content_hash algorithm

```
canonical_json = serialize(atom, sort_keys=true, omit=[
  "uuid",
  "aliases",
  "history",
  "ordering",
  "content_hash",
])
content_hash = "sha256:" + hex(sha256(canonical_json))
```

Excluding `uuid/aliases/history/ordering` means:
- rename does not change hash (cross-doc refs stay stable)
- view reordering does not change hash
- history append does not change hash (history is the audit, not the content)

Including `out_refs` in hash means:
- if atom A's refs change, its hash changes → all atoms referencing A see their dependency-hash changed → build-system style invalidation cascade

---

## Sub-path resolution (per atom kind)

Each kind defines its sub-path grammar. Initial set:

```
scenario:
  /step/<n>             — step N (1-indexed against yaml steps[])
  /silent/<n>           — silent block N
  /deeper/<n>           — deeper block N
  /title                — title only
  /summary              — summary only

invariant:
  /name                 — name only
  /summary

cmd:
  /flag/<name>          — specific flag
  /arg/<name>

prism:
  /trait/<name>         — find_refs / chunk / diff / etc.

path:
  /line/<n>             — file:line (跟 source-line tracking 一体)
  /function/<name>      — function-level (跟 prism rust ast 一体)

commit:
  /file/<path>          — what this commit did to a path
  /msg                  — message overlay (跟 scenario 85 一体)
```

Sub-paths are resolved by the kind crate, not by `alt-xref` core. New kinds plug in their resolver.

---

## Stones / steel / cement order

Following alt's stone/steel/cement methodology:

### Stones (independent crates · no business deps)

- **`alt-atom`** — atom data model · UUID v7 generation · semantic-id validation · alias chain · content_hash derivation · history append
- **`alt-xref`** — URL parser · resolver (with alias fallback + pin support) · sub-path dispatcher · failure-mode enum
- **`alt-sot`** — yaml loader · per-kind schema validation · cross-file uniqueness check · generated-file write-guard

### Steel (alt domain)

- `alt-cli` subcommands: `alt atom list/show/rename/deprecate/verify/rebuild` + `alt xref check/find/fix`
- integration with invariant 1 (rename/deprecate ops are signed)
- integration with invariant 2 (every atom op enters op log)
- integration with invariant 6 (immutable history per atom)
- prism trait extension: `find_refs(blob) → Vec<XrefUrl>` (each prism implements)

### Cement (application)

- `docs/design/sources/*.yaml` SoT files filled
- `docs/design/mental-model.html` converted to use typed refs throughout
- pre-commit hook (跟 scenario 31 一体): `alt xref check --strict` before commit
- CI gate: `alt xref check --ci` exit 1 on broken
- `docs/design/anti-rot-protocol.md` (this file) is itself an atom: `alt://principle/anti-rot`

---

## Phasing

| phase | scope | exit criterion |
|---|---|---|
| **0** · current feature branch | architecture doc (this file) · SoT spec · URL grammar · convert 5 typical refs in mental-model.html as dogfood proof. **No Rust code.** | this file commits + 5 typed refs working as plain HTML hyperlinks |
| **1** · M18 milestone | `alt-atom` stone · `alt-xref` stone · `alt-sot` stone (scaffolding) · 5 SoT files filled · `alt atom show/list` + `alt xref check` (verifier only) · CI gate | 0 broken refs in mental-model.html proven by `alt xref check --ci` |
| **2** · M18-M19 | `alt atom rename/deprecate/verify/rebuild` · prism `find_refs` trait · markdown/json/yaml/rust resolvers · `alt xref fix --interactive` · pre-commit integration · path stable uuid backend | rename `src/parser.rs` updates all refs in `.md`, `.json`, commit metadata atomically |
| **3** · M19-M20 | path / commit / issue / ci kinds full · external reconcile · visibility leak detection · backlinks graph · IDE extension (VS Code) · LLM augmentation point `xref.fix.suggest` (subject to invariant 7 LLM isolation) | issue tracker reconcile finds stale `closes #` refs on schedule |

---

## Open design questions (parking lot)

1. **UUID v7 vs v4** — v7 is time-ordered (sortable by creation) and contains a timestamp. v4 is pure random. v7 chosen — sortable beats privacy concern (atom yaml is git-tracked anyway, creation time leaks via git log).
2. **semantic id collision across kinds** — `alt://cmd/amend` ≠ `alt://scenario/amend`. Kind is part of the canonical identity. Verifier enforces uniqueness per-kind, not globally.
3. **i18n in semantic ids** — chinese characters in URL are url-encoding hell. **Decision**: semantic ids are ASCII-only kebab. `title` field holds chinese (or other) human display. Verifier rejects non-ASCII in `.id`.
4. **history truncation under disk pressure** — `.history` is append-only forever. **Decision**: never truncate per invariant 6. If disk becomes a problem, history moves to overlay store (跟 scenario 85 commit-msg overlay 同机制) — atom yaml keeps current state, overlay store keeps audit trail.
5. **forking / federation** — if two contributors each create an atom with same semantic id offline, merging will collide. **Decision**: at merge time, one is auto-renamed (semantic id + `-<short-uuid>` suffix), alias chain captures the conflict resolution. Same as git merge conflict on text — surfaces for human review.
6. **LLM-generated atoms** — agent might create atoms with hallucinated semantic ids. **Decision**: per invariant 7, LLM cannot mutate SoT directly. `xref.fix.suggest` augmentation point proposes; human or deterministic validator accepts.

---

## Maintenance log (this file)

This file is itself an atom (`alt://principle/anti-rot`) and is subject to gates 1 (audit) and 2 (redundancy) per `.claude/CLAUDE.md`.

- 2026-06-19 · created · architecture decisions captured per "你根据项目原则进行技术决策" directive · phase 0 only · no implementation yet
