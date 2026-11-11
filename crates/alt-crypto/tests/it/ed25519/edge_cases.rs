// CODEGEN: signatures built with big-integer curve arithmetic from known scalars and a point of order 8
//! Signatures whose key or R has a small-order component.

pub(super) struct Case {
    pub name: &'static str,
    pub key: &'static str,
    pub message: &'static str,
    pub signature: &'static str,
    /// whether `verify` accepts
    pub verify: bool,
    /// whether `verify_strict` accepts
    pub strict: bool,
}

pub(super) static CASES: [Case; 11] = [
    Case {
        name: "valid",
        key: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
        message: "656467652063617365",
        signature: "0de5fc7d5c24d555c15c8e3a1e4cccb9f8cde8bded8304e323cf257c6f483fbe\
                    7418851a0af989f2500050db044337d93d187ca17202df34e2fa2f1b52e7ca0d",
        verify: true,
        strict: true,
    },
    Case {
        name: "s_plus_group_order",
        key: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
        message: "656467652063617365",
        signature: "0de5fc7d5c24d555c15c8e3a1e4cccb9f8cde8bded8304e323cf257c6f483fbe\
                    61ec7a77245c9c4a279d477ee33c16ee3d187ca17202df34e2fa2f1b52e7ca1d",
        verify: false,
        strict: false,
    },
    Case {
        name: "r_identity_s_matches",
        key: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
        message: "656467652063617365",
        signature: "0100000000000000000000000000000000000000000000000000000000000000\
                    0d55939e7b6fc8d725b1d7aefc3ab5a750f4b0426a16d3aa1b190883447e6a03",
        verify: true,
        strict: false,
    },
    Case {
        name: "r_identity_as_y_p_plus_1",
        key: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
        message: "656467652063617365",
        signature: "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f\
                    6db2d4d0c8b8d071ae8f14eed1c0dce49a2b3f21fc6ab2b1be9e07839bfd140b",
        verify: false,
        strict: false,
    },
    Case {
        name: "r_identity_with_sign_bit",
        key: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
        message: "656467652063617365",
        signature: "0100000000000000000000000000000000000000000000000000000000000080\
                    4deb254e09af067f4660e4a1be9ad225e5a0caafd5d859a054c2789233b98b07",
        verify: false,
        strict: false,
    },
    Case {
        name: "mixed_order_key_torsion_cancels",
        key: "1f4f580e73ac208f06760190e9edc6f5916775dabd9c1cdca393175c2d6d1083",
        message: "6564676520636173652032",
        signature: "aed8b003abf2576baf9e02781168f1f5831d3d7d32f52edb28b1853ed384a9b9\
                    ac23ccf0aa84382610aac2fb5ce053f600764886e55da9503b1a2f4105b36e06",
        verify: true,
        strict: true,
    },
    Case {
        name: "mixed_order_key_torsion_remains",
        key: "1f4f580e73ac208f06760190e9edc6f5916775dabd9c1cdca393175c2d6d1083",
        message: "6564676520636173652030",
        signature: "29767242c3b23523ee3b766ccdc6803de8062b7254a00445384a423591076fd1\
                    c9535bf9bd9727a02dbbec3eec9ac77d6f4f16dc4fab51a482e24627c7d6c60c",
        verify: false,
        strict: false,
    },
    Case {
        name: "mixed_order_r",
        key: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
        message: "656467652063617365",
        signature: "018a56c250da15cf107154a1a2856b053049fdd9b613eff053a1730203d26731\
                    32408bbb062ee40efbccf70942c351674af81fdc317c5f9c6d79428860a8b804",
        verify: false,
        strict: false,
    },
    Case {
        name: "mixed_order_key_and_r_cancel",
        key: "1f4f580e73ac208f06760190e9edc6f5916775dabd9c1cdca393175c2d6d1083",
        message: "6564676520636173652030",
        signature: "36350e1e77f656d926c7c04d68b863bde11998eadbb02d934eb97423ea448ac9\
                    0ae59fc01be4f28ffc6ac21de5ebdf8a0f597617e2ddc0d44e800da9e94d6f0a",
        verify: true,
        strict: true,
    },
    Case {
        name: "small_order_key_mixed_r",
        key: "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
        message: "6564676520636173652030",
        signature: "bd58c0e366150bc482668824411034269efd7996dda53d2e48a198e0819a3170\
                    9d57e17f09ff04a88425f3bdac727026dddde9b10d6fd04a6b27170c8516de04",
        verify: true,
        strict: false,
    },
    Case {
        name: "r_order_8_s_matches_scalar_part",
        key: "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c",
        message: "656467652063617365",
        signature: "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a\
                    408ac8cecdb57e4c63d58e918e2d46cc736cdab1a82fae576f7b765ede1c290a",
        verify: false,
        strict: false,
    },
];
