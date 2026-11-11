mod json;

use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_remote_protocol::*;
use alt_repository_service::*;
use alt_repository_service::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

#[derive(Default)]
struct Objects(BTreeMap<ObjectId, (ObjectKind, Vec<u8>)>);
impl Objects {
    fn put(&mut self, kind: ObjectKind, bytes: impl AsRef<[u8]>) -> ObjectId {
        let bytes = bytes.as_ref();
        let oid = ObjectId::hash_object(HashAlgo::Sha1, kind, bytes);
        self.0.insert(oid, (kind, bytes.to_vec()));
        oid
    }
    fn tree(&mut self, entries: &[(&str, &[u8], ObjectId)]) -> ObjectId {
        let mut bytes = Vec::new();
        for (mode, name, oid) in entries {
            bytes.extend_from_slice(mode.as_bytes());
            bytes.push(b' ');
            bytes.extend_from_slice(name);
            bytes.push(0);
            bytes.extend_from_slice(oid.as_bytes());
        }
        self.put(ObjectKind::Tree, bytes)
    }
    fn commit(&mut self, tree: ObjectId, parents: &[ObjectId]) -> ObjectId {
        let mut bytes = format!("tree {tree}\n");
        for parent in parents {
            bytes.push_str(&format!("parent {parent}\n"));
        }
        bytes.push_str(
            "author A <a@example.test> 1 +0000\ncommitter A <a@example.test> 1 +0000\n\ntest\n",
        );
        self.put(ObjectKind::Commit, bytes)
    }
    fn graph(&self, roots: &[ObjectId]) -> Result<VerifiedGraph> {
        verify_graph(self, HashAlgo::Sha1, roots, graph_limits())
    }
}
impl ObjectSource for Objects {
    fn open(&self, oid: ObjectId) -> Result<Option<ObjectReader<'_>>> {
        Ok(self.0.get(&oid).map(|(kind, bytes)| ObjectReader {
            kind: *kind,
            bytes: bytes.len() as u64,
            reader: Box::new(Cursor::new(bytes)),
        }))
    }
}
fn graph_limits() -> GraphLimits {
    GraphLimits {
        objects: 1000,
        edges: 10000,
        object_bytes: 1_000_000,
        total_bytes: 10_000_000,
        structured_bytes: 100_000,
        paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
    }
}
fn wire_limits() -> Limits {
    Limits {
        request_bytes: 100000,
        ref_updates: 100,
        record_roots: 100,
        manifest_entries: 1000,
        graph_objects: 1000,
        graph_edges: 10000,
        structured_bytes: 1_000_000,
        tree_paths: 1000,
        path_bytes: 4096,
        walk_steps: 100_000,
        chunk_bytes: 1000,
        object_bytes: 1_000_000,
        total_bytes: 10_000_000,
    }
}
fn setup(
    old: Option<ObjectId>,
    new: Option<ObjectId>,
) -> (Publication, RepositorySnapshot, Authorization) {
    let scope = Scope {
        installation_id: "installation".into(),
        repository_instance_id: "repo".into(),
        recovery_epoch: 1,
    };
    let attribution = Attribution {
        kind: ActorKind::Human,
        on_behalf_of: None,
        delegation_id: None,
        agent_session: None,
    };
    let publication = Publication {
        protocol_version: VERSION,
        key: RequestKey {
            scope: scope.clone(),
            actor_id: "actor".into(),
            request_id: "request".into(),
        },
        attribution: attribution.clone(),
        object_format: ObjectFormat::Sha1,
        refs: vec![RefUpdate {
            name: "refs/heads/main".into(),
            expected: old.map(|o| o.to_string()),
            new: new.map(|o| o.to_string()),
            force: false,
        }],
        record_roots: vec![],
        content_manifest: "a".repeat(64),
    };
    let snapshot = RepositorySnapshot {
        scope: scope.clone(),
        runtime_generation: 1,
        revocation_generation: 1,
        policy_version: 1,
        object_format: ObjectFormat::Sha1,
        refs: old
            .into_iter()
            .map(|o| ("refs/heads/main".into(), o))
            .collect(),
        policies: vec![],
        approved_publications: BTreeSet::new(),
        writes_enabled: true,
    };
    let auth = Authorization {
        scope,
        runtime_generation: 1,
        actor_id: "actor".into(),
        attribution,
        expires_at: 100,
        revocation_generation: 1,
        grants: vec![Grant {
            refs: RefSelector::Namespace("refs/heads/".into()),
            create: true,
            update: true,
            delete: true,
            force: true,
            paths: None,
        }],
        publish_records: true,
    };
    (publication, snapshot, auth)
}
fn accepts(p: &Publication, s: &RepositorySnapshot, a: &Authorization, g: &VerifiedGraph) -> bool {
    validate_publication(p, s, a, g, wire_limits(), 10).is_ok()
}

#[test]
fn valid_closure_streams_blob_and_treats_gitlink_as_external() {
    let mut objects = Objects::default();
    let blob = objects.put(ObjectKind::Blob, vec![17; 200_000]);
    let external = ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Commit, b"external");
    let tree = objects.tree(&[("100644", b"a", blob), ("160000", b"submodule", external)]);
    let commit = objects.commit(tree, &[]);
    let graph = objects.graph(&[commit]).unwrap();
    assert_eq!(graph.objects().count(), 3);
    let (p, s, a) = setup(None, Some(commit));
    assert!(accepts(&p, &s, &a, &graph));
}
#[test]
fn missing_corrupt_and_type_confused_objects_are_rejected() {
    let mut objects = Objects::default();
    let blob = objects.put(ObjectKind::Blob, b"content");
    let tree = objects.tree(&[("40000", b"dir", blob)]);
    assert!(matches!(objects.graph(&[tree]), Err(Error::Integrity(_))));
    let commit = objects.commit(blob, &[]);
    assert!(objects.graph(&[commit]).is_err());
    objects.0.remove(&blob);
    assert!(matches!(objects.graph(&[tree]), Err(Error::Missing(_))));
    objects
        .0
        .insert(blob, (ObjectKind::Blob, b"changed".to_vec()));
    assert!(matches!(objects.graph(&[blob]), Err(Error::Integrity(_))));
}
#[test]
fn malformed_parent_duplicate_tree_and_tag_type_are_rejected() {
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, b"");
    for extra in [
        "parent invalid\n".to_string(),
        format!("tree {tree}\n"),
        format!("parent {}\n", "0".repeat(40)),
    ] {
        let commit = objects.put(
            ObjectKind::Commit,
            format!("tree {tree}\n{extra}author A\ncommitter B\n\nx"),
        );
        assert!(objects.graph(&[commit]).is_err());
    }
    let tag = objects.put(
        ObjectKind::Tag,
        format!("object {tree}\ntype commit\ntag v1\n\nx"),
    );
    assert!(objects.graph(&[tag]).is_err());
}
#[test]
fn unsafe_names_modes_duplicates_order_and_zero_oids_are_rejected() {
    let mut objects = Objects::default();
    let blob = objects.put(ObjectKind::Blob, b"");
    for name in [b"".as_slice(), b"..", b"a/b", b".git", b".ALT"] {
        let tree = objects.tree(&[("100644", name, blob)]);
        assert!(objects.graph(&[tree]).is_err());
    }
    for entries in [
        vec![("100644", b"a".as_slice(), blob), ("100755", b"a", blob)],
        vec![("100644", b"z".as_slice(), blob), ("100644", b"a", blob)],
        vec![("777777", b"a".as_slice(), blob)],
        vec![("160000", b"a".as_slice(), ObjectId::Sha1([0; 20]))],
    ] {
        let tree = objects.tree(&entries);
        assert!(objects.graph(&[tree]).is_err());
    }
}
#[test]
fn graph_limits_apply_before_unbounded_read_or_expansion() {
    let mut objects = Objects::default();
    let blob = objects.put(ObjectKind::Blob, vec![0; 20]);
    let tree = objects.tree(&[("100644", b"a", blob)]);
    let mut limits = graph_limits();
    limits.object_bytes = 10;
    assert!(matches!(
        verify_graph(&objects, HashAlgo::Sha1, &[blob], limits),
        Err(Error::Limit(_))
    ));
    limits = graph_limits();
    limits.objects = 1;
    assert!(matches!(
        verify_graph(&objects, HashAlgo::Sha1, &[tree], limits),
        Err(Error::Limit(_))
    ));
    limits = graph_limits();
    limits.structured_bytes = 10;
    assert!(matches!(
        verify_graph(&objects, HashAlgo::Sha1, &[tree], limits),
        Err(Error::Limit(_))
    ));
}
#[test]
fn branch_type_cas_fast_forward_force_and_delete_are_distinct() {
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, b"");
    let base = objects.commit(tree, &[]);
    let next = objects.commit(tree, &[base]);
    let graph = objects.graph(&[base, next]).unwrap();
    let (mut p, mut s, mut a) = setup(Some(base), Some(next));
    assert!(accepts(&p, &s, &a, &graph));
    s.refs.clear();
    assert!(!accepts(&p, &s, &a, &graph));
    s.refs.insert("refs/heads/main".into(), base);
    p.refs[0].new = Some(tree.to_string());
    assert!(!accepts(&p, &s, &a, &graph));
    let (mut p, s, mut a2) = setup(Some(next), Some(base));
    assert!(!accepts(&p, &s, &a2, &graph));
    p.refs[0].force = true;
    assert!(accepts(&p, &s, &a2, &graph));
    a2.grants[0].force = false;
    assert!(!accepts(&p, &s, &a2, &graph));
    p.refs[0].new = None;
    p.refs[0].force = false;
    assert!(accepts(&p, &s, &a, &graph));
    a.grants[0].delete = false;
    assert!(!accepts(&p, &s, &a, &graph));
}
#[test]
fn identity_lease_epoch_generation_and_attribution_are_checked() {
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, b"");
    let commit = objects.commit(tree, &[]);
    let graph = objects.graph(&[commit]).unwrap();
    let (p, s, a) = setup(None, Some(commit));
    for index in 0..7 {
        let mut a = a.clone();
        match index {
            0 => a.scope.installation_id = "other".into(),
            1 => a.scope.recovery_epoch += 1,
            2 => a.runtime_generation += 1,
            3 => a.revocation_generation += 1,
            4 => a.expires_at = 10,
            5 => a.actor_id = "other".into(),
            _ => a.attribution.kind = ActorKind::Service,
        }
        assert!(!accepts(&p, &s, &a, &graph));
    }
}
#[test]
fn protected_ref_approval_is_bound_to_exact_digest() {
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, b"");
    let commit = objects.commit(tree, &[]);
    let graph = objects.graph(&[commit]).unwrap();
    let (mut p, mut s, a) = setup(None, Some(commit));
    s.policies.push(RefPolicy {
        refs: RefSelector::Exact("refs/heads/main".into()),
        allow_force: false,
        allow_delete: false,
        require_review: true,
    });
    assert!(!accepts(&p, &s, &a, &graph));
    s.approved_publications
        .insert(p.digest(wire_limits()).unwrap());
    assert!(accepts(&p, &s, &a, &graph));
    p.key.request_id = "different".into();
    assert!(!accepts(&p, &s, &a, &graph));
}
#[test]
fn path_scope_checks_intermediate_history_not_just_tip_diff() {
    let mut objects = Objects::default();
    let first = objects.put(ObjectKind::Blob, b"first");
    let changed = objects.put(ObjectKind::Blob, b"secret change");
    let t1 = objects.tree(&[("100644", b"allowed", first), ("100644", b"secret", first)]);
    let t2 = objects.tree(&[
        ("100644", b"allowed", first),
        ("100644", b"secret", changed),
    ]);
    let base = objects.commit(t1, &[]);
    let forbidden = objects.commit(t2, &[base]);
    let reverted = objects.commit(t1, &[forbidden]);
    let graph = objects.graph(&[reverted]).unwrap();
    assert!(
        graph
            .changed_paths(Some(base), Some(reverted))
            .unwrap()
            .is_empty()
    );
    let (p, s, mut a) = setup(Some(base), Some(reverted));
    assert!(accepts(&p, &s, &a, &graph));
    a.grants[0].paths = Some(vec![b"allowed".to_vec()]);
    assert!(!accepts(&p, &s, &a, &graph));
}
#[test]
fn raw_paths_modes_and_prefix_boundaries_survive_path_checks() {
    let mut objects = Objects::default();
    let blob = objects.put(ObjectKind::Blob, b"same");
    let t1 = objects.tree(&[("100644", b"\xff", blob)]);
    let t2 = objects.tree(&[("100755", b"\xff", blob)]);
    let base = objects.commit(t1, &[]);
    let next = objects.commit(t2, &[base]);
    let graph = objects.graph(&[next]).unwrap();
    assert_eq!(
        graph.changed_paths(Some(base), Some(next)).unwrap(),
        vec![vec![255]]
    );
    let (p, s, mut a) = setup(Some(base), Some(next));
    a.grants[0].paths = Some(vec![vec![255]]);
    assert!(accepts(&p, &s, &a, &graph));
    a.grants[0].paths = Some(vec![b"other".to_vec()]);
    assert!(!accepts(&p, &s, &a, &graph));
}
#[test]
fn reference_namespace_collision_is_atomic_precondition() {
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, b"");
    let commit = objects.commit(tree, &[]);
    let graph = objects.graph(&[commit]).unwrap();
    let (p, mut s, a) = setup(None, Some(commit));
    s.refs.insert("refs/heads/main/child".into(), commit);
    assert!(!accepts(&p, &s, &a, &graph));
    assert!(s.refs.contains_key("refs/heads/main/child"));
    assert!(!s.refs.contains_key("refs/heads/main"));
}

#[test]
fn malformed_policy_and_shared_traversal_budget_fail_closed() {
    let mut objects = Objects::default();
    let tree = objects.put(ObjectKind::Tree, b"");
    let base = objects.commit(tree, &[]);
    let next = objects.commit(tree, &[base]);
    let mut limits = graph_limits();
    limits.walk_steps = 1;
    let graph = verify_graph(&objects, HashAlgo::Sha1, &[next], limits).unwrap();
    let (p, mut s, a) = setup(Some(base), Some(next));
    assert!(!accepts(&p, &s, &a, &graph));
    let graph = objects.graph(&[next]).unwrap();
    assert!(accepts(&p, &s, &a, &graph));
    s.policies.push(RefPolicy {
        refs: RefSelector::Namespace("refs/heads".into()),
        allow_force: false,
        allow_delete: false,
        require_review: true,
    });
    assert!(!accepts(&p, &s, &a, &graph));
}

/// a repository that already holds `held`; reading one of those objects is a failure
struct Settled<'a> {
    new: &'a Objects,
    held: &'a Objects,
}
impl ObjectSource for Settled<'_> {
    fn open(&self, oid: ObjectId) -> Result<Option<ObjectReader<'_>>> {
        assert!(!self.held.0.contains_key(&oid), "a settled object was read");
        self.new.open(oid)
    }
    fn settled(&self, oid: ObjectId) -> Result<Option<ObjectKind>> {
        Ok(self.held.0.get(&oid).map(|(kind, _)| *kind))
    }
}
impl Graph for Objects {
    fn node(&self, oid: ObjectId) -> Result<std::borrow::Cow<'_, Node>> {
        let (kind, bytes) = self.0.get(&oid).ok_or(Error::Missing(oid))?;
        parse_node(*kind, bytes, HashAlgo::Sha1).map(std::borrow::Cow::Owned)
    }
    fn graph_limits(&self) -> GraphLimits {
        graph_limits()
    }
}

#[test]
fn verification_stops_at_settled_objects_and_checks_walk_on_below_them() {
    let mut held = Objects::default();
    let old_blob = held.put(ObjectKind::Blob, b"old");
    let old_tree = held.tree(&[("100644", b"kept", old_blob)]);
    let root = held.commit(old_tree, &[]);
    let tip = held.commit(old_tree, &[root]);
    let mut new = Objects::default();
    let new_blob = new.put(ObjectKind::Blob, b"new");
    let new_tree = new.tree(&[
        ("100644", b"added", new_blob),
        ("100644", b"kept", old_blob),
    ]);
    let next = new.commit(new_tree, &[tip]);
    let source = Settled {
        new: &new,
        held: &held,
    };
    let graph = verify_new_graph(&source, HashAlgo::Sha1, &[tip, next], graph_limits()).unwrap();
    let read: BTreeSet<_> = graph.objects().collect();
    assert_eq!(read, BTreeSet::from([new_blob, new_tree, next]));
    assert!(graph.holds(tip) && graph.holds(old_blob) && graph.holds(next));
    // below the boundary nothing was looked at
    assert!(!graph.holds(root) && !graph.holds(old_tree));
    assert_eq!(graph.reachable(next).unwrap().len(), 3);
    assert!(graph.reachable(tip).unwrap().is_empty());

    // a fast-forward from a commit two below the boundary: the walk has to continue
    // in what the repository holds
    let (p, s, a) = setup(Some(root), Some(next));
    let layered = Layered {
        verified: &graph,
        settled: &held,
    };
    assert!(validate_publication(&p, &s, &a, &layered, wire_limits(), 10).is_ok());
    assert!(validate_publication(&p, &s, &a, &graph, wire_limits(), 10).is_err());
    // and it is still a real ancestry check
    let mut other = Objects::default();
    let other_tree = other.tree(&[("100644", b"elsewhere", old_blob)]);
    let unrelated = other.commit(other_tree, &[]);
    held.0.extend(other.0);
    let source = Settled {
        new: &new,
        held: &held,
    };
    let graph =
        verify_new_graph(&source, HashAlgo::Sha1, &[unrelated, next], graph_limits()).unwrap();
    let (p, s, a) = setup(Some(unrelated), Some(next));
    let layered = Layered {
        verified: &graph,
        settled: &held,
    };
    assert!(validate_publication(&p, &s, &a, &layered, wire_limits(), 10).is_err());

    // a settled object named with the wrong type is refused like a read one
    let confused = new.tree(&[("40000", b"kept", old_blob)]);
    let bad = new.commit(confused, &[tip]);
    let source = Settled {
        new: &new,
        held: &held,
    };
    assert!(matches!(
        verify_new_graph(&source, HashAlgo::Sha1, &[bad], graph_limits()),
        Err(Error::Integrity("dependency type mismatch"))
    ));
    // the full verification reads everything
    let mut all = Objects::default();
    all.0.extend(held.0.clone());
    all.0.extend(new.0.clone());
    assert_eq!(all.graph(&[next]).unwrap().objects().count(), 7);
}
