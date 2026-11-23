//! The statements a Git view is read with.

/// Whether the caller may read the repository, as a condition of each read of its
/// references: asked by anyone else, the statement does not look at them.
macro_rules! readable {
    () => {
        "EXISTS(SELECT 1 FROM alt_grants g JOIN alt_credentials c ON c.actor_id=g.actor_id WHERE c.token_digest=$1 AND g.repository=$2 AND g.can_read)"
    };
}
/// One statement per way of reading the references, so the plain reads carry no
/// filter. `$refs` is the aggregated reference text and `$others` whether a reference
/// points outside the object ids of the filter.
macro_rules! view {
    ($refs:expr, $others:expr) => {
        concat!(
            "SELECT
 (SELECT row_to_json(i) FROM (SELECT installation_id,workspace_id,deployment_mode,runtime_generation,signing_key_id FROM alt_instance WHERE singleton) i),
 (SELECT row_to_json(c) FROM (SELECT actor_id,attribution,identity_source,expires_at,revoked FROM alt_credentials WHERE token_digest=$1) c),
 EXISTS(SELECT 1 FROM alt_delegations WHERE token_digest=$1),
 (SELECT row_to_json(a) FROM (SELECT disabled FROM alt_web_accounts WHERE actor_id=(SELECT actor_id FROM alt_credentials WHERE token_digest=$1)) a),
 (SELECT row_to_json(s) FROM (SELECT s.account_generation,a.generation,s.expires_at FROM alt_web_sessions s JOIN alt_web_accounts a ON a.actor_id=s.actor_id WHERE s.token_digest=$1) s),
 (SELECT row_to_json(g) FROM (SELECT can_read,publish_records,grants FROM alt_grants WHERE repository=$2 AND actor_id=(SELECT actor_id FROM alt_credentials WHERE token_digest=$1)) g),
 (SELECT row_to_json(r) FROM (SELECT recovery_epoch,object_format,default_branch,pack_unreachable,writes_enabled AND portable_freeze IS NULL AND portable_import IS NULL AS writable FROM alt_repositories WHERE id=$2) r),
 ",
            $refs,
            ",
 (SELECT coalesce(json_agg(json_build_array(segment_id,inline IS NOT NULL) ORDER BY sequence),'[]'::json) FROM alt_pack_segments WHERE repository=$2 AND state='live'),
 floor(extract(epoch FROM clock_timestamp())*1000)::bigint,
 ",
            $others
        )
    };
}
pub(super) const VIEW_ALL: &str = view!(
    concat!(
        "(SELECT string_agg(oid||' '||name,E'\\n') FROM alt_refs WHERE repository=$2 AND ",
        readable!(),
        ")"
    ),
    "false"
);
pub(super) const VIEW_NOTHING: &str = view!("NULL::text", "false");
// one more row than `DEFERRED_REFS`, written out: a limit passed as a parameter makes
// the server plan the statement again on every execution
pub(super) const VIEW_BOUNDED: &str = view!(
    concat!(
        "(SELECT string_agg(oid||' '||name,E'\\n') FROM (SELECT oid,name FROM alt_refs WHERE repository=$2 AND ",
        readable!(),
        " LIMIT 4097) r)"
    ),
    "false"
);
pub(super) const VIEW_PREFIXES: &str = view!(
    concat!(
        "(SELECT string_agg(oid||' '||name,E'\\n') FROM alt_refs WHERE repository=$2 AND ",
        readable!(),
        " AND (name=(SELECT default_branch FROM alt_repositories WHERE id=$2) OR EXISTS(SELECT 1 FROM unnest($3::text[]) prefix WHERE starts_with(name,prefix))))"
    ),
    "false"
);
// Both are planned for the object ids they are given: a plan made for any array
// tests a row against it one element at a time. Planning for an array hashes it,
// which is faster to run than the subquery and slower to plan as the array grows.
pub(super) const VIEW_TIPS: &str = view!(
    concat!(
        "(SELECT string_agg(oid||' '||name,E'\\n') FROM alt_refs WHERE repository=$2 AND ",
        readable!(),
        " AND (oid=ANY($3::text[]) OR ($4::bool AND starts_with(name,'refs/tags/'))))"
    ),
    concat!(
        "EXISTS(SELECT 1 FROM alt_refs WHERE repository=$2 AND ",
        readable!(),
        " AND NOT oid=ANY($3::text[]))"
    )
);
pub(super) const VIEW_MANY_TIPS: &str = view!(
    concat!(
        "(SELECT string_agg(oid||' '||name,E'\\n') FROM alt_refs WHERE repository=$2 AND ",
        readable!(),
        " AND (oid IN (SELECT unnest($3::text[])) OR ($4::bool AND starts_with(name,'refs/tags/'))))"
    ),
    concat!(
        "EXISTS(SELECT 1 FROM alt_refs WHERE repository=$2 AND ",
        readable!(),
        " AND oid NOT IN (SELECT unnest($3::text[])))"
    )
);
