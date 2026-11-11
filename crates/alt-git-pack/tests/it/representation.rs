use alt_git_codec::{HashAlgo, ObjectId, ObjectKind};
use alt_git_pack::{
    DeltaOptions, IngressLimits, NoBases, PackWriter, RepresentationLimits, ingest_pack,
    ingest_pack_with_representations,
};
use std::{
    fs::File,
    io::Cursor,
    process::{Command, Stdio},
};

fn limits() -> IngressLimits {
    IngressLimits {
        wire_bytes: 1 << 20,
        objects: 100,
        object_bytes: 1 << 18,
        inflated_bytes: 1 << 20,
        resolved_bytes: 1 << 20,
        delta_depth: 16,
        steps: 10000,
    }
}

fn retention() -> RepresentationLimits {
    RepresentationLimits {
        bytes: 1 << 20,
        objects: 100,
        object_bytes: 1 << 18,
    }
}

fn plain_pack(algo: HashAlgo, objects: &[Vec<u8>]) -> Vec<u8> {
    let mut pack = b"PACK".to_vec();
    pack.extend_from_slice(&2u32.to_be_bytes());
    pack.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    for data in objects {
        let mut size = data.len();
        let mut byte = (3 << 4) | (size & 15) as u8;
        size >>= 4;
        while size > 0 {
            pack.push(byte | 128);
            byte = (size & 127) as u8;
            size >>= 7;
        }
        pack.push(byte);
        // A non-default encoding proves that output preserves supplied bytes.
        pack.extend_from_slice(&alt_deflate::compress(data, 0, alt_deflate::Format::Zlib));
    }
    let trailer = match algo {
        HashAlgo::Sha1 => alt_hash::sha1(&pack).to_vec(),
        HashAlgo::Sha256 => alt_hash::sha256(&pack).to_vec(),
    };
    pack.extend_from_slice(&trailer);
    pack
}

#[test]
fn retained_representations_preserve_wire_bytes_and_pass_independent_git() {
    for (format, algo) in [("sha1", HashAlgo::Sha1), ("sha256", HashAlgo::Sha256)] {
        let data = vec![
            Vec::new(),
            vec![42; 4096],
            (0..70001).map(|i| i as u8).collect(),
        ];
        let raw = plain_pack(algo, &data);
        let decoded = ingest_pack_with_representations(
            &mut Cursor::new(&raw),
            algo,
            limits(),
            &mut NoBases,
            retention(),
        )
        .unwrap();
        assert_eq!(decoded.representations.len(), data.len());
        let output = alt_os::temp::tempdir().unwrap();
        let mut writer = PackWriter::create_with_deltas(
            output.path(),
            algo,
            data.len() as u32,
            DeltaOptions::default(),
        )
        .unwrap();
        for (object, representation) in decoded.objects.iter().zip(&decoded.representations) {
            assert!(
                writer
                    .add_reusing(object.oid, object.kind, &object.data, representation)
                    .unwrap()
            );
        }
        let written = writer.finish().unwrap();
        assert_eq!(std::fs::read(&written.pack_path).unwrap(), raw);
        let repository = alt_os::temp::tempdir().unwrap();
        let init = Command::new("git")
            .args(["init", "--bare"])
            .arg(format!("--object-format={format}"))
            .arg(repository.path())
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        let index = Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["index-pack", "--stdin", "--strict"])
            .stdin(Stdio::from(File::open(&written.pack_path).unwrap()))
            .output()
            .unwrap();
        assert!(
            index.status.success(),
            "{}",
            String::from_utf8_lossy(&index.stderr)
        );
        for original in &data {
            let oid = ObjectId::hash_object(algo, ObjectKind::Blob, original);
            let object = Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(["cat-file", "blob", &oid.to_string()])
                .output()
                .unwrap();
            assert!(object.status.success());
            assert_eq!(object.stdout, *original);
        }
        // Reuse failure must occur before any header/data is emitted, so the
        // original caller can still write a valid different object.
        let output = alt_os::temp::tempdir().unwrap();
        let mut writer = PackWriter::create(output.path(), algo, 1).unwrap();
        let different = b"current bytes changed";
        let oid = ObjectId::hash_object(algo, ObjectKind::Blob, different);
        assert!(
            !writer
                .add_reusing(
                    oid,
                    ObjectKind::Blob,
                    different,
                    &decoded.representations[1]
                )
                .unwrap()
        );
        let written = writer.finish().unwrap();
        let restored = ingest_pack(
            &mut File::open(written.pack_path).unwrap(),
            algo,
            limits(),
            &mut NoBases,
        )
        .unwrap();
        assert_eq!(restored.objects[0].data.as_slice(), different);
    }
}

#[test]
fn retention_bounds_never_change_decoded_objects_and_do_not_retain_deltas() {
    let data = vec![Vec::new(), vec![3; 4096], vec![8; 65537]];
    let raw = plain_pack(HashAlgo::Sha1, &data);
    for (bounds, count) in [
        (RepresentationLimits::default(), 0),
        (
            RepresentationLimits {
                objects: 1,
                ..retention()
            },
            1,
        ),
        (
            RepresentationLimits {
                bytes: 20,
                ..retention()
            },
            1,
        ),
        (
            RepresentationLimits {
                object_bytes: 20,
                ..retention()
            },
            1,
        ),
        (retention(), 3),
    ] {
        let decoded = ingest_pack_with_representations(
            &mut Cursor::new(&raw),
            HashAlgo::Sha1,
            limits(),
            &mut NoBases,
            bounds,
        )
        .unwrap();
        assert_eq!(decoded.representations.len(), count);
        assert!(
            decoded
                .representations
                .iter()
                .map(|r| r.encoded_bytes())
                .sum::<usize>()
                <= bounds.bytes
        );
        for (object, original) in decoded.objects.iter().zip(&data) {
            assert_eq!(object.data.as_slice(), original);
        }
    }
    let output = alt_os::temp::tempdir().unwrap();
    let mut writer =
        PackWriter::create_with_deltas(output.path(), HashAlgo::Sha1, 2, DeltaOptions::default())
            .unwrap();
    let mut original: Vec<_> = (0..65536).map(|i| (i * 17 + i / 257) as u8).collect();
    for i in 0..2 {
        original[901] = i;
        writer
            .add(
                ObjectId::hash_object(HashAlgo::Sha1, ObjectKind::Blob, &original),
                ObjectKind::Blob,
                &original,
            )
            .unwrap();
    }
    let written = writer.finish().unwrap();
    let decoded = ingest_pack_with_representations(
        &mut File::open(written.pack_path).unwrap(),
        HashAlgo::Sha1,
        limits(),
        &mut NoBases,
        retention(),
    )
    .unwrap();
    assert_eq!(decoded.objects.len(), 2);
    assert_eq!(decoded.representations.len(), 1);
}
