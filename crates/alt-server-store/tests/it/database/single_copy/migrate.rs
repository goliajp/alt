//! Upgrading a schema 18 instance, which kept every natively published Git object
//! in the content store as well.
use super::*;
use alt_portable::BlobSource;

type Item<'a> = (&'a ContentEntry, &'a Vec<u8>);

fn git_items(payload: &Payload) -> Vec<Item<'_>> {
    payload
        .manifest
        .entries
        .iter()
        .zip(&payload.contents)
        .filter(|(entry, _)| entry.git_oid.is_some())
        .collect()
}

/// Puts Git objects into the content store the way schema 18 left published ones:
/// stored, counted, mapped to their object id and retained by `pin`.
fn keep_in_content_store(f: &Fixture, scope: &Scope, items: &[Item<'_>], pin: &str) {
    let mut client = f.client();
    for (entry, bytes) in items {
        f.store
            .bytes()
            .put(
                scope,
                &entry.blake3,
                entry.bytes,
                limits().object_bytes,
                &mut Cursor::new(bytes),
            )
            .unwrap();
        let counted = client
            .query_opt(
                "SELECT accounted FROM alt_blobs WHERE blake3=$1",
                &[&entry.blake3],
            )
            .unwrap()
            .is_some_and(|row| row.get(0));
        let kind = serde_json::to_value(entry.kind).unwrap();
        client.execute("INSERT INTO alt_blobs(repository,blake3,bytes,state,accounted,created_at) VALUES('repository',$1,$2,'stored',true,0) ON CONFLICT(repository,blake3) DO UPDATE SET state='stored',accounted=true", &[&entry.blake3,&(entry.bytes as i64)]).unwrap();
        if !counted {
            client
                .execute(
                    "UPDATE alt_repositories SET stored_bytes=stored_bytes+$1",
                    &[&(entry.bytes as i64)],
                )
                .unwrap();
        }
        client.execute("INSERT INTO alt_objects(repository,oid,kind,blake3) VALUES('repository',$1,$2,$3) ON CONFLICT DO NOTHING", &[&entry.git_oid,&kind.as_str(),&entry.blake3]).unwrap();
        client.execute("INSERT INTO alt_pins(repository,pin_id,blake3,expires_at) VALUES('repository',$1,$2,NULL) ON CONFLICT DO NOTHING", &[&pin,&entry.blake3]).unwrap();
    }
}

/// what the migration moves or leaves, as one comparable value
fn state(f: &Fixture) -> serde_json::Value {
    f.client().query_one("SELECT jsonb_build_object('pins',(SELECT jsonb_agg(to_jsonb(p) ORDER BY pin_id,blake3) FROM alt_pins p),'blobs',(SELECT jsonb_agg(jsonb_build_array(blake3,state,accounted) ORDER BY blake3) FROM alt_blobs),'objects',(SELECT jsonb_agg(to_jsonb(o) ORDER BY oid) FROM alt_objects o),'segments',(SELECT jsonb_agg(jsonb_build_array(segment_id,state,objects,bytes) ORDER BY segment_id) FROM alt_pack_segments),'stored',(SELECT jsonb_agg(stored_bytes ORDER BY id) FROM alt_repositories),'refs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY name) FROM alt_refs r))", &[]).unwrap().get(0)
}
fn count(f: &Fixture, sql: &str) -> i64 {
    f.client().query_one(sql, &[]).unwrap().get(0)
}
/// object mappings, pack segments, objects in them, bytes counted
fn numbers(f: &Fixture) -> (i64, i64, i64, i64) {
    (
        count(f, "SELECT count(*) FROM alt_objects"),
        count(
            f,
            "SELECT count(*) FROM alt_pack_segments WHERE state='live'",
        ),
        count(
            f,
            "SELECT COALESCE(sum(objects),0)::bigint FROM alt_pack_segments WHERE state='live'",
        ),
        count(
            f,
            "SELECT stored_bytes FROM alt_repositories WHERE id='repository'",
        ),
    )
}
fn segment_files(f: &Fixture) -> usize {
    std::fs::read_dir(f.store.pack_directory("repository").unwrap())
        .unwrap()
        .count()
}
/// makes the database the schema 18 one it stands in for
fn as_schema_18(f: &Fixture) {
    f.client()
        .batch_execute("DELETE FROM alt_schema WHERE version=19; ALTER TABLE alt_pins DROP CONSTRAINT IF EXISTS alt_pins_no_ref")
        .unwrap();
}
fn migrate(f: &mut Fixture) -> alt_server_store::Result<()> {
    as_schema_18(f);
    let store = Store::migrate(
        f.client(),
        f.store.bytes().clone(),
        f.store.spec().clone(),
        SigningKey::from_bytes(&f.signing_seed),
        limits(),
        18,
    )?;
    f.spec = store.spec().clone();
    f.store = store;
    f.credential = f.store.authenticate(&f.token).unwrap();
    Ok(())
}
fn on_branch(mut payload: Payload, name: &str) -> Payload {
    payload.publication.refs[0].name = name.into();
    payload
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn a_schema_18_repository_is_settled_into_its_pack_segments() {
    let mut f = Fixture::new();
    let scope = Payload::new("scope", None).publication.key.scope;
    // a native publication as schema 18 left it: in a segment, and in the content
    // store under the pin of its ref
    let native = commit_with_file("old-native", b"published natively", None);
    native.publish_new(&mut f.store, &f.credential);
    // objects that are in a segment only, as a git push leaves them
    let pushed = commit_with_file("old-pushed", b"pushed", Some(native.commit));
    pushed.publish_new(&mut f.store, &f.credential);
    // a task publication: its Git objects pinned with the bundle, like its records
    let task = super::super::tasks::task_payload("old-task");
    publish_task(&mut f, &task);
    let bundle = format!(
        "task-bundle:{}",
        task.manifest.entries.last().unwrap().blake3
    );
    // bytes that are an attachment of that task and a file of a commit
    let at = task
        .manifest
        .entries
        .iter()
        .position(|entry| entry.kind == ContentKind::Attachment)
        .unwrap();
    let shared = on_branch(
        commit_with_file("old-shared", &task.contents[at], None),
        "refs/heads/shared",
    );
    shared.publish_new(&mut f.store, &f.credential);
    // an export that is open across the upgrade
    let export = f
        .store
        .prepare_portable_export(
            &f.credential,
            &scope,
            "open-export",
            super::super::portable::portable_limits(),
        )
        .unwrap();
    let exported = export.envelope().catalog.blobs.clone();
    drop(export);

    as_schema_18(&f);
    keep_in_content_store(&f, &scope, &git_items(&native), "ref:refs/heads/main");
    keep_in_content_store(
        &f,
        &scope,
        &git_items(&native),
        "portable-export:open-export",
    );
    keep_in_content_store(&f, &scope, &git_items(&task), &bundle);
    keep_in_content_store(&f, &scope, &git_items(&shared), "ref:refs/heads/shared");
    // an object a task publication carried outside its graph: no segment has it
    let raw = b"carried outside the graph by an old task publication".to_vec();
    let loose = git_entry(
        ContentKind::Blob,
        ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &raw),
        &raw,
    );
    keep_in_content_store(&f, &scope, &[(&loose, &raw)], &bundle);
    // and an upload in flight, which is nobody's published object yet
    let flying = on_branch(
        commit_with_file("old-flying", b"still uploading", None),
        "refs/heads/flying",
    );
    flying.upload(&mut f.store, &f.credential);

    let before = numbers(&f);
    assert_eq!((before.0, before.1, before.2), (12, 4, 11));
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'ref:%'"
        ),
        6
    );
    let untouched = state(&f);

    // a failure late in the transaction leaves the instance as it was
    f.client().batch_execute("CREATE FUNCTION stop_migration() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected migration failure'; END $$; CREATE TRIGGER stop_migration BEFORE UPDATE ON alt_instance FOR EACH ROW EXECUTE FUNCTION stop_migration()").unwrap();
    assert!(migrate(&mut f).is_err());
    assert_eq!(state(&f), untouched);
    assert_eq!(
        f.client()
            .query_one("SELECT max(version) FROM alt_schema", &[])
            .unwrap()
            .get::<_, i32>(0),
        18
    );
    assert_eq!(segment_files(&f), 4);
    f.client()
        .batch_execute(
            "DROP TRIGGER stop_migration ON alt_instance; DROP FUNCTION stop_migration()",
        )
        .unwrap();
    migrate(&mut f).unwrap();
    let after = numbers(&f);
    println!(
        "schema 18 -> 19: object mappings {} -> {}, live segments {} -> {}, objects in segments {} -> {}, stored bytes {} -> {}",
        before.0, after.0, before.1, after.1, before.2, after.2, before.3, after.3
    );

    // one new segment with the one object that was not in any
    assert_eq!((after.1, after.2), (5, 12));
    assert_eq!(segment_files(&f), 5);
    // every object reads as before, by object id and by content digest
    for payload in [&native, &pushed, &task, &shared] {
        assert_readable(&mut f, payload);
    }
    let (_, size, mut reader) = f
        .store
        .open_published_object(
            &f.credential,
            "repository",
            loose.git_oid.as_ref().unwrap().parse().unwrap(),
        )
        .unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!((size, &bytes), (raw.len() as u64, &raw));
    // no ref records retention; what is only a Git object has no pin and no copy
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM alt_pins WHERE pin_id LIKE 'ref:%'"
        ),
        0
    );
    for payload in [&native, &pushed] {
        assert_eq!(git_rows(&f, payload), (0, 0, 0));
    }
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM alt_blobs b WHERE state='stored' AND EXISTS(SELECT 1 FROM alt_pins p WHERE p.blake3=b.blake3 AND p.expires_at IS NULL)"
        ),
        5,
        "the record, the three attachments and the bundle"
    );
    // the attachment keeps its copy and the pin of its bundle
    assert_eq!(
        f.client()
            .query(
                "SELECT pin_id FROM alt_pins WHERE blake3=$1 AND expires_at IS NULL ORDER BY pin_id",
                &[&task.manifest.entries[at].blake3]
            )
            .unwrap()
            .iter()
            .map(|row| row.get::<_, String>(0))
            .collect::<Vec<_>>(),
        vec!["portable-export:open-export".to_owned(), bundle.clone()]
    );
    // the only object mappings left belong to the upload in flight
    assert_eq!(after.0, 3);
    assert_eq!(git_rows(&f, &flying), (3, 3, 3));
    assert_accounted(&f);

    // the open export still serves everything it listed
    let export = f
        .store
        .open_portable_export(
            &f.credential,
            &scope,
            "open-export",
            super::super::portable::portable_limits().package,
        )
        .unwrap();
    for blob in &exported {
        let mut bytes = Vec::new();
        export.open(blob).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(alt_portable::Blob::of(&bytes), *blob);
    }
    drop(export);

    // running it again changes nothing
    let settled = state(&f);
    migrate(&mut f).unwrap();
    assert_eq!(state(&f), settled);

    // the upload that was in flight publishes, and so does new work
    flying.publish(&mut f.store, &f.credential);
    let next = commit_with_file("old-next", b"pushed", Some(pushed.commit));
    next.publish_new(&mut f.store, &f.credential);
    assert_readable(&mut f, &flying);
    assert_readable(&mut f, &next);
    assert_eq!(count(&f, "SELECT count(*) FROM alt_objects"), 0);
    assert_accounted(&f);
}

#[test]
#[ignore = "requires isolated PostgreSQL via scripts/with-test-postgres.sh"]
fn an_import_activated_before_segments_is_settled_into_one() {
    let mut f = Fixture::new();
    let (source, export, intent) = super::super::portable::import_fixture(&f);
    super::super::portable::stage_import(&mut f, &export, &intent);
    let work = super::super::portable::verify_import(&mut f, &intent);
    f.store
        .activate_portable_import(&f.credential, &work)
        .unwrap();
    drop(work);
    // schema 18 activations left the Git objects in the content store alone
    let catalog = &export.envelope().catalog;
    let mut contents = Vec::new();
    for object in &catalog.objects {
        let mut bytes = Vec::new();
        export
            .open(&object.payload)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        contents.push((
            ContentEntry {
                kind: object.kind,
                blake3: object.payload.blake3.clone(),
                bytes: object.payload.bytes,
                git_oid: Some(object.oid.clone()),
                chunks: vec![],
            },
            bytes,
        ));
    }
    as_schema_18(&f);
    let pin: String = f
        .client()
        .query_one(
            "SELECT pin_id FROM alt_pins WHERE pin_id LIKE 'portable-import:%' LIMIT 1",
            &[],
        )
        .unwrap()
        .get(0);
    let items: Vec<Item<'_>> = contents
        .iter()
        .map(|(entry, bytes)| (entry, bytes))
        .collect();
    keep_in_content_store(&f, &intent.key.scope, &items, &pin);
    let segment: String = f
        .client()
        .query_one("SELECT segment_id FROM alt_pack_segments", &[])
        .unwrap()
        .get(0);
    std::fs::remove_file(f.store.pack_path("repository", &segment).unwrap()).unwrap();
    f.client()
        .execute("DELETE FROM alt_pack_segments", &[])
        .unwrap();
    let before = numbers(&f);
    assert_eq!((before.0, before.1), (catalog.objects.len() as i64, 0));
    let retained = count(&f, "SELECT count(*) FROM alt_pins WHERE expires_at IS NULL");

    migrate(&mut f).unwrap();
    let after = numbers(&f);
    println!(
        "imported schema 18 -> 19: object mappings {} -> {}, live segments {} -> {}, objects in segments {} -> {}, stored bytes {} -> {}",
        before.0, after.0, before.1, after.1, before.2, after.2, before.3, after.3
    );
    assert_eq!(
        (after.0, after.1, after.2),
        (0, 1, catalog.objects.len() as i64)
    );
    for (entry, bytes) in &contents {
        let (_, size, mut reader) = f
            .store
            .open_published_object(
                &f.credential,
                "repository",
                entry.git_oid.as_ref().unwrap().parse().unwrap(),
            )
            .unwrap();
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).unwrap();
        assert_eq!((size, &actual), (bytes.len() as u64, bytes));
    }
    // the documents, receipts and task content keep their pins; the objects have none
    assert_eq!(
        count(&f, "SELECT count(*) FROM alt_pins WHERE expires_at IS NULL"),
        retained - catalog.objects.len() as i64
    );
    assert_accounted(&f);
    // what was imported exports again, complete
    let scope = intent.key.scope.clone();
    let again = f
        .store
        .prepare_portable_export(
            &f.credential,
            &scope,
            "after-upgrade",
            super::super::portable::portable_limits(),
        )
        .unwrap();
    assert_eq!(again.envelope().catalog.objects, catalog.objects);
    alt_portable::VerifiedSnapshot::verify(
        again.envelope(),
        &scope,
        SigningKey::from_bytes(&f.signing_seed)
            .verifying_key()
            .as_bytes(),
        &again,
        super::super::portable::portable_limits(),
    )
    .unwrap();
    drop(source);
}
