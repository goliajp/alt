use alt_hash::{Sha1, Sha256, sha1, sha1_many, sha256};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn noise(len: usize, mut state: u64) -> Vec<u8> {
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

#[test]
fn sha1_published_vectors() {
    assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(
        hex(&sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        hex(&sha1(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );
    assert_eq!(
        hex(&sha1(&vec![b'a'; 1_000_000])),
        "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
    );
}

#[test]
fn sha1_accelerated_matches_portable_at_every_length() {
    let data = noise(1024, 0x9e37_79b9_7f4a_7c15);
    for len in 0..data.len() {
        // portable reference: pad by hand, then fold with the reference compression
        let mut padded = data[..len].to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&((len as u64) * 8).to_be_bytes());
        let mut state = [
            0x6745_2301u32,
            0xefcd_ab89,
            0x98ba_dcfe,
            0x1032_5476,
            0xc3d2_e1f0,
        ];
        alt_hash::sha1::portable(&mut state, &padded);
        let expected: Vec<u8> = state.iter().flat_map(|word| word.to_be_bytes()).collect();
        assert_eq!(sha1(&data[..len]).to_vec(), expected, "length {len}");
    }
}

#[test]
fn sha1_streaming_splits_do_not_matter() {
    let data = noise(5000, 7);
    let whole = sha1(&data);
    for step in [1, 3, 63, 64, 65, 127, 1000] {
        let mut hasher = Sha1::new();
        for chunk in data.chunks(step) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.finalize(), whole, "step {step}");
    }
}

#[test]
fn sha1_agrees_with_git_object_ids() {
    for (index, len) in [0usize, 1, 55, 56, 64, 4096, 300_001]
        .into_iter()
        .enumerate()
    {
        let data = noise(len, 11 + index as u64);
        let mut child = Command::new("git")
            .args(["hash-object", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&data).unwrap();
        let out = child.wait_with_output().unwrap();
        let mut hasher = Sha1::new();
        hasher.update(format!("blob {len}\0").as_bytes());
        hasher.update(&data);
        assert_eq!(
            hex(&hasher.finalize()),
            String::from_utf8(out.stdout).unwrap().trim()
        );
    }
}

#[test]
fn sha1_many_matches_one_at_a_time() {
    // lengths around every padding boundary, uneven sizes, and counts that do not fill the lanes
    for count in [0usize, 1, 3, 4, 7, 8, 9, 17, 100] {
        let bodies: Vec<Vec<u8>> = (0..count)
            .map(|index| {
                noise(
                    (index * 37
                        + [0, 1, 55, 56, 63, 64, 65, 119, 120, 128, 1000, 70_000][index % 12])
                        % 80_000,
                    100 + index as u64,
                )
            })
            .collect();
        let headers: Vec<String> = bodies
            .iter()
            .map(|body| format!("blob {}\0", body.len()))
            .collect();
        let parts: Vec<[&[u8]; 2]> = headers
            .iter()
            .zip(&bodies)
            .map(|(header, body)| [header.as_bytes(), body.as_slice()])
            .collect();
        let messages: Vec<&[&[u8]]> = parts.iter().map(|pair| pair.as_slice()).collect();
        let together = sha1_many(&messages);
        assert_eq!(together.len(), count);
        for (index, digest) in together.iter().enumerate() {
            let mut hasher = Sha1::new();
            hasher.update(headers[index].as_bytes());
            hasher.update(&bodies[index]);
            assert_eq!(
                *digest,
                hasher.finalize(),
                "count {count} message {index} length {}",
                bodies[index].len()
            );
        }
    }
}

#[test]
fn sha256_published_vectors() {
    assert_eq!(
        hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    assert_eq!(
        hex(&sha256(&vec![b'a'; 1_000_000])),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn sha256_accelerated_matches_portable_at_every_length_and_split() {
    let data = noise(1024, 0x1234_5678_9abc_def1);
    for len in 0..data.len() {
        let mut padded = data[..len].to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&((len as u64) * 8).to_be_bytes());
        let mut state = [
            0x6a09_e667u32,
            0xbb67_ae85,
            0x3c6e_f372,
            0xa54f_f53a,
            0x510e_527f,
            0x9b05_688c,
            0x1f83_d9ab,
            0x5be0_cd19,
        ];
        alt_hash::sha256::portable(&mut state, &padded);
        let expected: Vec<u8> = state.iter().flat_map(|word| word.to_be_bytes()).collect();
        assert_eq!(sha256(&data[..len]).to_vec(), expected, "length {len}");
    }
    let whole = sha256(&data);
    for step in [1, 3, 63, 64, 65, 127, 1000] {
        let mut hasher = Sha256::new();
        for chunk in data.chunks(step) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.finalize(), whole, "step {step}");
    }
}

/// Prints which implementations the tests above exercised: on a machine without the
/// SHA instructions the suite is run again under an emulator that has them.
#[test]
fn reports_the_selected_implementations() {
    println!(
        "sha1: {}, sha256: {}",
        alt_hash::sha1::implementation(),
        alt_hash::sha256::implementation()
    );
}
