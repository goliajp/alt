//! The deflate prism on arbitrary bytes: parsing an untrusted file's
//! embedded zlib stream must never panic or bomb (the inflate output is
//! bounded), and the iron law must hold — anything `decompose` accepts must
//! recompose to the exact input.
#![no_main]

use alt_prism::Prism;
use alt_prism_deflate::DeflatePrism;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(d) = DeflatePrism.decompose(data) {
        let parts: Vec<&[u8]> = d.parts.iter().map(Vec::as_slice).collect();
        assert_eq!(
            DeflatePrism.recompose(&d.recipe, &parts).as_deref(),
            Some(data),
            "a decomposition the prism accepted must reproduce the input",
        );
    }
});
