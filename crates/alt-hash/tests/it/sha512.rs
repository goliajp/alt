use super::{hex, noise};
use alt_hash::{Sha512, sha512};

#[test]
fn published_vectors() {
    assert_eq!(
        hex(&sha512(b"")),
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
    );
    assert_eq!(
        hex(&sha512(b"abc")),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(
        hex(&sha512(
            b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
        )),
        "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
    );
    assert_eq!(
        hex(&sha512(&vec![b'a'; 1_000_000])),
        "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
    );
}

/// digests of the first `n` bytes of `noise(1100, 0x9e37_79b9_7f4a_7c15)`
static BOUNDARIES: [(usize, &str); 16] = [
    (
        0,
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
    ),
    (
        1,
        "e5f7f597c756ff6f6efc09eb14ed7bea3b63be7efa5798c45f4978f0471ee8899a3025adbf91fe71608e7cc3e80e5e3a45793eba1dd1cc90e74b20f2ac3c187c",
    ),
    (
        55,
        "1b741b566077fde90856ba339d2f2d81889774b06fa1266f89ba3985e2ceaf0318edf2348d9d3483c575b50fee319c4766751039101021edfc6a99614c08e30b",
    ),
    (
        111,
        "4be46b33598b8cfea04a97b5259f9e1a0773ec7a10d282a6d1aac3c5b642e73879376f1ece600f58abc45b6c04dfc2b5f7be184da7537decc961f2f7b1554156",
    ),
    (
        112,
        "65641f6a642170a26014b4e7fea3b208452a694ea7216209a5b59a06322dcf25c7d46a9c759926a2c42edabb341db57e5ce26c07a5796e9f35545a18902c5637",
    ),
    (
        113,
        "80b53dcaba5e91b5208dd5f2ad7013f87fe13696e3c9b1871f21b385420e42a4326871eab50754bb74e426777f94a1bc90095cd10425ec3874c2e7f93c9adcc0",
    ),
    (
        127,
        "05701988b820194bac7ec3cee66cd02935a6f8e0b026221141b92ba32d58efbde25b42fcfc6308a5a4e49cebd36392b445425afc288bb61990b6881c18741f12",
    ),
    (
        128,
        "1d1adfda3beb4adfd3efc226cd3f45032512a5a0fef7357c89fe62889d4a564295fcecb8bd29a9ebbfcde5fc9f6039169fa486e35e166acfe395e6e60c7d5f27",
    ),
    (
        129,
        "654a705c0b553b4e11ab53beeafe75b35e0bb06a2f91de82d2871f785189d3b6eae37623559fede7d414323b8782cdb9fe2c28875198cf10e4204dbbb4ad3c1c",
    ),
    (
        239,
        "2968cb6e0b8c9b44de4af739c430a7d5f06fb1854c15d6f20f9e5829ee0fbb0776714441130fe975d8be7c797d51424c41c2a5475d31bb8fe17bc0ea82a4c545",
    ),
    (
        240,
        "0d20c7fb36c351c8bc988bb65772fa906af7189082f1ae03e633ae61b407d1e4b1962528f5cc9eba4d1036cc2339a2f90dfc569755363b34b5936b61c797c6f5",
    ),
    (
        241,
        "4c7d3ff53e4671c81115ec39e037acea7bde0412e469bd10bb79781874c51845f09446a3d93abbb10b9e7addce96ea03cb283fb95bfa033de0f00cc713740eb3",
    ),
    (
        255,
        "c39e90f748f1f3efeee86ef9f4bb12a891779d68d32685934a7363511077092865c6ab35bd2390ae0cce87dfe5ac40b7359dd1e12f4b82912be85605d28ec097",
    ),
    (
        256,
        "12199e107a98ec40c8b63abec93468b3ea57508daac6a469c4ba66a8d690936fc55b2eda2b0d7ab44801adfe7865a6a1f7910cd0ca07963ab8da87dd46fb2b45",
    ),
    (
        257,
        "d74a41d96ad4025d0634269a7cb09c954851dec4182f6edcc2d05e5360650523ffb34e4aeaf92362976e82d73f93d045b5bc5394ad92b5b69fb82309e356b468",
    ),
    (
        1000,
        "f5bb752ec3bb17a6305cbf01f75c9433a6792331b86685a01722429f7956788c85f2a337f88a2f1bdc84da048bb90533121323022c3d757ee1e177639f973466",
    ),
];

#[test]
fn padding_boundaries() {
    let data = noise(1100, 0x9e37_79b9_7f4a_7c15);
    for (len, expected) in BOUNDARIES {
        assert_eq!(hex(&sha512(&data[..len])), expected, "length {len}");
    }
}

#[test]
fn accelerated_matches_portable_at_every_length() {
    let data = noise(1024, 0x9e37_79b9_7f4a_7c15);
    for len in 0..data.len() {
        // portable reference: pad by hand, then fold with the reference compression
        let mut padded = data[..len].to_vec();
        padded.push(0x80);
        while padded.len() % 128 != 112 {
            padded.push(0);
        }
        padded.extend_from_slice(&((len as u128) * 8).to_be_bytes());
        let mut state = alt_hash::sha512::INITIAL;
        alt_hash::sha512::portable(&mut state, &padded);
        let expected: Vec<u8> = state.iter().flat_map(|word| word.to_be_bytes()).collect();
        assert_eq!(sha512(&data[..len]).to_vec(), expected, "length {len}");
    }
}

#[test]
fn streaming_splits_do_not_matter() {
    let data = noise(5000, 7);
    let whole = sha512(&data);
    for step in [1, 3, 127, 128, 129, 255, 1000] {
        let mut hasher = Sha512::new();
        for chunk in data.chunks(step) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.finalize(), whole, "step {step}");
    }
}

#[test]
fn reports_an_implementation() {
    let name = alt_hash::sha512::implementation();
    println!("sha512: {name}");
    assert!(["sha512 instructions", "neon", "avx2", "portable"].contains(&name));
}
