// CODEGEN: tools/gen_tables.py over the WHATWG Encoding Standard's data
// files https://encoding.spec.whatwg.org/encodings.json and
// https://encoding.spec.whatwg.org/index-<name>.txt, then rustfmt; do not
// edit by hand.

//! The encodings of the Encoding Standard (§4) and their labels.

/// An encoding of the Encoding Standard, in the order of its table (§4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Encoding {
    /// `UTF-8`
    Utf8,
    /// `IBM866`
    Ibm866,
    /// `ISO-8859-2`
    Iso8859_2,
    /// `ISO-8859-3`
    Iso8859_3,
    /// `ISO-8859-4`
    Iso8859_4,
    /// `ISO-8859-5`
    Iso8859_5,
    /// `ISO-8859-6`
    Iso8859_6,
    /// `ISO-8859-7`
    Iso8859_7,
    /// `ISO-8859-8`
    Iso8859_8,
    /// `ISO-8859-8-I`
    Iso8859_8I,
    /// `ISO-8859-10`
    Iso8859_10,
    /// `ISO-8859-13`
    Iso8859_13,
    /// `ISO-8859-14`
    Iso8859_14,
    /// `ISO-8859-15`
    Iso8859_15,
    /// `ISO-8859-16`
    Iso8859_16,
    /// `KOI8-R`
    Koi8R,
    /// `KOI8-U`
    Koi8U,
    /// `macintosh`
    Macintosh,
    /// `windows-874`
    Windows874,
    /// `windows-1250`
    Windows1250,
    /// `windows-1251`
    Windows1251,
    /// `windows-1252`
    Windows1252,
    /// `windows-1253`
    Windows1253,
    /// `windows-1254`
    Windows1254,
    /// `windows-1255`
    Windows1255,
    /// `windows-1256`
    Windows1256,
    /// `windows-1257`
    Windows1257,
    /// `windows-1258`
    Windows1258,
    /// `x-mac-cyrillic`
    XMacCyrillic,
    /// `GBK`
    Gbk,
    /// `gb18030`
    Gb18030,
    /// `Big5`
    Big5,
    /// `EUC-JP`
    EucJp,
    /// `ISO-2022-JP`
    Iso2022Jp,
    /// `Shift_JIS`
    ShiftJis,
    /// `EUC-KR`
    EucKr,
    /// `replacement`
    Replacement,
    /// `UTF-16BE`
    Utf16be,
    /// `UTF-16LE`
    Utf16le,
    /// `x-user-defined`
    XUserDefined,
}

impl Encoding {
    /// Every encoding, in discriminant order.
    pub const ALL: [Encoding; 40] = [
        Encoding::Utf8,
        Encoding::Ibm866,
        Encoding::Iso8859_2,
        Encoding::Iso8859_3,
        Encoding::Iso8859_4,
        Encoding::Iso8859_5,
        Encoding::Iso8859_6,
        Encoding::Iso8859_7,
        Encoding::Iso8859_8,
        Encoding::Iso8859_8I,
        Encoding::Iso8859_10,
        Encoding::Iso8859_13,
        Encoding::Iso8859_14,
        Encoding::Iso8859_15,
        Encoding::Iso8859_16,
        Encoding::Koi8R,
        Encoding::Koi8U,
        Encoding::Macintosh,
        Encoding::Windows874,
        Encoding::Windows1250,
        Encoding::Windows1251,
        Encoding::Windows1252,
        Encoding::Windows1253,
        Encoding::Windows1254,
        Encoding::Windows1255,
        Encoding::Windows1256,
        Encoding::Windows1257,
        Encoding::Windows1258,
        Encoding::XMacCyrillic,
        Encoding::Gbk,
        Encoding::Gb18030,
        Encoding::Big5,
        Encoding::EucJp,
        Encoding::Iso2022Jp,
        Encoding::ShiftJis,
        Encoding::EucKr,
        Encoding::Replacement,
        Encoding::Utf16be,
        Encoding::Utf16le,
        Encoding::XUserDefined,
    ];

    /// The encoding's name, as the standard spells it.
    pub const fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Ibm866 => "IBM866",
            Encoding::Iso8859_2 => "ISO-8859-2",
            Encoding::Iso8859_3 => "ISO-8859-3",
            Encoding::Iso8859_4 => "ISO-8859-4",
            Encoding::Iso8859_5 => "ISO-8859-5",
            Encoding::Iso8859_6 => "ISO-8859-6",
            Encoding::Iso8859_7 => "ISO-8859-7",
            Encoding::Iso8859_8 => "ISO-8859-8",
            Encoding::Iso8859_8I => "ISO-8859-8-I",
            Encoding::Iso8859_10 => "ISO-8859-10",
            Encoding::Iso8859_13 => "ISO-8859-13",
            Encoding::Iso8859_14 => "ISO-8859-14",
            Encoding::Iso8859_15 => "ISO-8859-15",
            Encoding::Iso8859_16 => "ISO-8859-16",
            Encoding::Koi8R => "KOI8-R",
            Encoding::Koi8U => "KOI8-U",
            Encoding::Macintosh => "macintosh",
            Encoding::Windows874 => "windows-874",
            Encoding::Windows1250 => "windows-1250",
            Encoding::Windows1251 => "windows-1251",
            Encoding::Windows1252 => "windows-1252",
            Encoding::Windows1253 => "windows-1253",
            Encoding::Windows1254 => "windows-1254",
            Encoding::Windows1255 => "windows-1255",
            Encoding::Windows1256 => "windows-1256",
            Encoding::Windows1257 => "windows-1257",
            Encoding::Windows1258 => "windows-1258",
            Encoding::XMacCyrillic => "x-mac-cyrillic",
            Encoding::Gbk => "GBK",
            Encoding::Gb18030 => "gb18030",
            Encoding::Big5 => "Big5",
            Encoding::EucJp => "EUC-JP",
            Encoding::Iso2022Jp => "ISO-2022-JP",
            Encoding::ShiftJis => "Shift_JIS",
            Encoding::EucKr => "EUC-KR",
            Encoding::Replacement => "replacement",
            Encoding::Utf16be => "UTF-16BE",
            Encoding::Utf16le => "UTF-16LE",
            Encoding::XUserDefined => "x-user-defined",
        }
    }

    /// The index a legacy single-byte encoding decodes with.
    pub fn single_byte_index(self) -> Option<&'static crate::single_byte::Index> {
        use crate::index::*;
        Some(match self {
            Encoding::Ibm866 => &IBM866,
            Encoding::Iso8859_2 => &ISO_8859_2,
            Encoding::Iso8859_3 => &ISO_8859_3,
            Encoding::Iso8859_4 => &ISO_8859_4,
            Encoding::Iso8859_5 => &ISO_8859_5,
            Encoding::Iso8859_6 => &ISO_8859_6,
            Encoding::Iso8859_7 => &ISO_8859_7,
            Encoding::Iso8859_8 => &ISO_8859_8,
            Encoding::Iso8859_8I => &ISO_8859_8,
            Encoding::Iso8859_10 => &ISO_8859_10,
            Encoding::Iso8859_13 => &ISO_8859_13,
            Encoding::Iso8859_14 => &ISO_8859_14,
            Encoding::Iso8859_15 => &ISO_8859_15,
            Encoding::Iso8859_16 => &ISO_8859_16,
            Encoding::Koi8R => &KOI8_R,
            Encoding::Koi8U => &KOI8_U,
            Encoding::Macintosh => &MACINTOSH,
            Encoding::Windows874 => &WINDOWS_874,
            Encoding::Windows1250 => &WINDOWS_1250,
            Encoding::Windows1251 => &WINDOWS_1251,
            Encoding::Windows1252 => &WINDOWS_1252,
            Encoding::Windows1253 => &WINDOWS_1253,
            Encoding::Windows1254 => &WINDOWS_1254,
            Encoding::Windows1255 => &WINDOWS_1255,
            Encoding::Windows1256 => &WINDOWS_1256,
            Encoding::Windows1257 => &WINDOWS_1257,
            Encoding::Windows1258 => &WINDOWS_1258,
            Encoding::XMacCyrillic => &X_MAC_CYRILLIC,
            _ => return None,
        })
    }
}

/// The labels' perfect hash multipliers, one per eight bytes of key.
pub(crate) const LABEL_HASH: [u64; 3] =
    [0xF3B8488C368CB0A7, 0x657EECDD3CB13D09, 0xC2D326E0055BDEF7];

/// Each hash bucket's displacement.
pub(crate) static LABEL_DISPLACEMENTS: [u8; 128] = [
    5, 4, 0, 3, 0, 17, 4, 0, 3, 0, 3, 0, 0, 0, 1, 8, 12, 0, 1, 0, 8, 6, 6, 20, 15, 5, 8, 8, 1, 0,
    1, 0, 12, 6, 1, 0, 3, 0, 3, 2, 44, 1, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 11, 4, 0, 0, 0, 1, 0, 64,
    0, 64, 0, 0, 3, 16, 8, 13, 2, 0, 4, 1, 0, 8, 0, 85, 0, 65, 1, 3, 18, 0, 1, 0, 2, 2, 81, 3, 10,
    10, 7, 0, 0, 14, 0, 2, 33, 21, 5, 75, 0, 0, 95, 34, 3, 0, 100, 20, 0, 0, 1, 28, 2, 16, 9, 2,
    48, 8, 17, 19, 1, 0, 5, 15, 9, 0, 50, 0,
];

/// Each slot's label (its start in [`LABEL_BYTES`] and its length,
/// `u8::MAX` for an empty slot) and that label's encoding.
pub(crate) static LABEL_SLOTS: [(u16, u8, Encoding); 256] = [
    (0, 10, Encoding::Iso8859_15),
    (10, 11, Encoding::Iso8859_10),
    (21, 9, Encoding::Windows1252),
    (30, 8, Encoding::Utf16be),
    (38, 6, Encoding::Windows1254),
    (44, 12, Encoding::Iso8859_7),
    (56, 9, Encoding::Windows874),
    (65, 12, Encoding::Iso8859_8I),
    (77, 8, Encoding::Windows1252),
    (85, 10, Encoding::Windows874),
    (95, 6, Encoding::Iso8859_3),
    (101, 15, Encoding::Iso8859_7),
    (116, 11, Encoding::Replacement),
    (127, 15, Encoding::Iso8859_7),
    (142, 6, Encoding::Iso8859_2),
    (148, 2, Encoding::Iso8859_4),
    (150, 9, Encoding::Iso8859_8),
    (159, 5, Encoding::Windows1252),
    (164, 15, Encoding::Utf16le),
    (179, 2, Encoding::Iso8859_15),
    (181, 7, Encoding::Koi8U),
    (188, 7, Encoding::Gbk),
    (195, 9, Encoding::Windows1254),
    (204, 8, Encoding::Utf16le),
    (212, 6, Encoding::Iso8859_6),
    (218, 6, Encoding::EucJp),
    (224, 11, Encoding::Iso2022Jp),
    (235, 15, Encoding::Iso8859_3),
    (250, 6, Encoding::Utf16le),
    (256, 10, Encoding::Windows1252),
    (266, 5, Encoding::Windows1252),
    (271, 10, Encoding::Iso8859_7),
    (281, 7, Encoding::Gbk),
    (288, 6, Encoding::Iso8859_8),
    (294, 11, Encoding::Iso8859_10),
    (305, 13, Encoding::EucKr),
    (318, 9, Encoding::Iso8859_10),
    (327, 6, Encoding::Windows1252),
    (333, 6, Encoding::EucKr),
    (339, 10, Encoding::Iso8859_5),
    (349, 18, Encoding::Iso8859_5),
    (367, 11, Encoding::Iso8859_8I),
    (378, 10, Encoding::Iso8859_3),
    (388, 3, Encoding::Koi8R),
    (391, 8, Encoding::Windows1255),
    (399, 8, Encoding::Windows1258),
    (407, 6, Encoding::Gbk),
    (413, 10, Encoding::EucKr),
    (423, 10, Encoding::Windows1254),
    (433, 2, Encoding::Windows1254),
    (435, 8, Encoding::Windows1251),
    (443, 11, Encoding::Macintosh),
    (454, 10, Encoding::Iso8859_8),
    (464, 11, Encoding::Iso8859_15),
    (475, 11, Encoding::Iso8859_3),
    (486, 6, Encoding::Koi8R),
    (492, 8, Encoding::Windows1250),
    (500, 8, Encoding::Windows1257),
    (508, 8, Encoding::Windows1254),
    (516, 11, Encoding::Iso8859_14),
    (527, 8, Encoding::Windows1256),
    (535, 8, Encoding::Windows1253),
    (543, 12, Encoding::Windows1251),
    (555, 10, Encoding::ShiftJis),
    (565, 15, Encoding::Iso8859_6),
    (580, 9, Encoding::Iso8859_3),
    (589, 11, Encoding::EucKr),
    (600, 16, Encoding::Iso8859_6),
    (616, 9, Encoding::Iso8859_13),
    (625, 2, Encoding::Iso8859_3),
    (627, 11, Encoding::Iso8859_4),
    (638, 9, Encoding::Macintosh),
    (647, 6, Encoding::EucKr),
    (653, 11, Encoding::Macintosh),
    (664, 4, Encoding::ShiftJis),
    (668, 10, Encoding::Iso8859_14),
    (678, 10, Encoding::Replacement),
    (688, 9, Encoding::Utf16le),
    (697, 7, Encoding::Big5),
    (704, 7, Encoding::EucKr),
    (711, 8, Encoding::Iso8859_5),
    (719, 11, Encoding::Iso8859_6),
    (730, 10, Encoding::Iso8859_6),
    (740, 11, Encoding::Windows874),
    (751, 15, Encoding::Iso8859_2),
    (766, 10, Encoding::Iso8859_4),
    (776, 11, Encoding::Utf16be),
    (787, 6, Encoding::Iso8859_4),
    (793, 8, Encoding::Iso8859_7),
    (801, 10, Encoding::Iso8859_5),
    (811, 11, Encoding::ShiftJis),
    (822, 8, Encoding::Iso8859_6),
    (830, 10, Encoding::Iso8859_6),
    (840, 6, Encoding::Windows1252),
    (846, 3, Encoding::Ibm866),
    (849, 7, Encoding::Koi8R),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (856, 9, Encoding::Iso8859_2),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (865, 10, Encoding::Windows1254),
    (875, 12, Encoding::Windows1256),
    (887, 11, Encoding::Iso8859_6),
    (898, 12, Encoding::Windows1255),
    (910, 6, Encoding::Iso8859_10),
    (916, 10, Encoding::Iso8859_8),
    (926, 10, Encoding::Iso8859_2),
    (0, u8::MAX, Encoding::Replacement),
    (936, 6, Encoding::Iso8859_7),
    (942, 8, Encoding::Gbk),
    (950, 12, Encoding::Iso8859_6),
    (962, 7, Encoding::Utf16le),
    (969, 5, Encoding::Iso8859_7),
    (974, 10, Encoding::Iso8859_2),
    (984, 10, Encoding::Iso8859_10),
    (994, 11, Encoding::Iso2022Jp),
    (0, u8::MAX, Encoding::Replacement),
    (1005, 8, Encoding::EucKr),
    (0, u8::MAX, Encoding::Replacement),
    (1013, 3, Encoding::Macintosh),
    (1016, 15, Encoding::XMacCyrillic),
    (1031, 12, Encoding::Iso8859_6),
    (1043, 6, Encoding::Koi8U),
    (1049, 12, Encoding::Windows1252),
    (1061, 7, Encoding::EucKr),
    (0, u8::MAX, Encoding::Replacement),
    (1068, 15, Encoding::Iso8859_5),
    (0, u8::MAX, Encoding::Replacement),
    (1083, 10, Encoding::Gbk),
    (1093, 8, Encoding::Windows1252),
    (1101, 10, Encoding::Windows1254),
    (1111, 14, Encoding::EucKr),
    (1125, 11, Encoding::Iso8859_15),
    (1136, 14, Encoding::EucKr),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (1150, 15, Encoding::Replacement),
    (1165, 16, Encoding::Iso8859_8),
    (1181, 10, Encoding::Iso8859_13),
    (0, u8::MAX, Encoding::Replacement),
    (1191, 15, Encoding::Windows1252),
    (1206, 9, Encoding::Gbk),
    (1215, 5, Encoding::Ibm866),
    (1220, 10, Encoding::Iso8859_3),
    (1230, 10, Encoding::Iso8859_5),
    (1240, 10, Encoding::Iso8859_3),
    (1250, 11, Encoding::Replacement),
    (1261, 5, Encoding::ShiftJis),
    (1266, 9, Encoding::Iso8859_5),
    (1275, 2, Encoding::Iso8859_2),
    (1277, 9, Encoding::Iso8859_15),
    (1286, 5, Encoding::Gbk),
    (1291, 10, Encoding::Iso8859_7),
    (1301, 15, Encoding::Gbk),
    (1316, 11, Encoding::Windows1254),
    (1327, 4, Encoding::Utf8),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (1331, 7, Encoding::Windows874),
    (1338, 10, Encoding::Windows1252),
    (0, u8::MAX, Encoding::Replacement),
    (1348, 12, Encoding::Windows1253),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (1360, 11, Encoding::Windows1252),
    (1371, 12, Encoding::Windows1257),
    (1383, 10, Encoding::Iso8859_7),
    (0, u8::MAX, Encoding::Replacement),
    (0, u8::MAX, Encoding::Replacement),
    (1393, 5, Encoding::Utf8),
    (1398, 9, Encoding::Iso8859_14),
    (1407, 10, Encoding::Windows1252),
    (1417, 6, Encoding::Koi8R),
    (1423, 8, Encoding::Iso8859_6),
    (1431, 9, Encoding::Iso8859_4),
    (1440, 10, Encoding::Iso8859_10),
    (1450, 7, Encoding::Iso8859_8I),
    (1457, 19, Encoding::EucJp),
    (0, u8::MAX, Encoding::Replacement),
    (1476, 11, Encoding::Replacement),
    (1487, 13, Encoding::Utf8),
    (1500, 4, Encoding::Koi8R),
    (0, u8::MAX, Encoding::Replacement),
    (1504, 15, Encoding::Utf8),
    (1519, 11, Encoding::Windows874),
    (1530, 8, Encoding::Iso8859_7),
    (1538, 10, Encoding::Big5),
    (1548, 8, Encoding::Iso8859_4),
    (1556, 2, Encoding::Windows1252),
    (1558, 6, Encoding::Windows1256),
    (0, u8::MAX, Encoding::Replacement),
    (1564, 13, Encoding::Utf8),
    (1577, 15, Encoding::Iso8859_4),
    (1592, 15, Encoding::Iso8859_8),
    (1607, 6, Encoding::Windows1250),
    (1613, 6, Encoding::Windows1253),
    (1619, 11, Encoding::Iso8859_16),
    (1630, 5, Encoding::Utf16le),
    (1635, 14, Encoding::XUserDefined),
    (1649, 11, Encoding::Iso8859_15),
    (1660, 3, Encoding::Gbk),
    (1663, 6, Encoding::Windows1254),
    (1669, 9, Encoding::Iso8859_7),
    (1678, 10, Encoding::Iso8859_2),
    (1688, 10, Encoding::Iso8859_8),
    (1698, 4, Encoding::Big5),
    (1702, 10, Encoding::Iso8859_4),
    (1712, 12, Encoding::Windows1258),
    (1724, 11, Encoding::Utf16le),
    (1735, 6, Encoding::Windows1255),
    (1741, 6, Encoding::Windows1257),
    (1747, 6, Encoding::Windows1258),
    (1753, 6, Encoding::Windows1251),
    (1759, 17, Encoding::Utf8),
    (1776, 8, Encoding::ShiftJis),
    (1784, 6, Encoding::Windows1252),
    (1790, 11, Encoding::Replacement),
    (1801, 6, Encoding::Ibm866),
    (1807, 9, Encoding::ShiftJis),
    (1816, 10, Encoding::Iso8859_6),
    (1826, 11, Encoding::Iso8859_8),
    (1837, 8, Encoding::Iso8859_2),
    (1845, 11, Encoding::Iso8859_2),
    (1856, 6, Encoding::ShiftJis),
    (1862, 12, Encoding::Windows1254),
    (1874, 8, Encoding::Ibm866),
    (1882, 10, Encoding::Iso8859_4),
    (1892, 8, Encoding::Iso8859_7),
    (1900, 8, Encoding::EucJp),
    (1908, 11, Encoding::Iso8859_13),
    (1919, 12, Encoding::Iso8859_8),
    (1931, 7, Encoding::Gb18030),
    (1938, 9, Encoding::ShiftJis),
    (1947, 8, Encoding::Iso8859_5),
    (1955, 8, Encoding::Iso8859_3),
    (1963, 8, Encoding::Windows1254),
    (1971, 14, Encoding::Windows1252),
    (1985, 7, Encoding::Windows874),
    (1992, 8, Encoding::Iso8859_8),
    (2000, 15, Encoding::Windows1254),
    (2015, 6, Encoding::Iso8859_8),
    (2021, 2, Encoding::Iso8859_10),
    (2023, 14, Encoding::XMacCyrillic),
    (2037, 8, Encoding::Iso8859_6),
    (2045, 6, Encoding::Big5),
    (2051, 9, Encoding::Iso8859_6),
    (2060, 12, Encoding::Windows1250),
    (2072, 8, Encoding::Big5),
    (2080, 8, Encoding::Windows1252),
];

/// Every label in slot order, unseparated, then 24 zero bytes so a
/// 24-byte read from any label's start stays in bounds.
pub(crate) static LABEL_BYTES: &[u8] = b"\
    iso8859-15iso-8859-10iso8859-1utf-16belatin5sun_eu_greekiso885911iso-8859-8-ix-c\
    p1252iso8859-11latin3csisolatingreekiso-2022-cniso_8859-7:1987latin2l4iso8859-8a\
    sciiiso-10646-ucs-2l9koi8-rugb_2312iso8859-9utf-16learabiceuc-jpiso-2022-jpiso_8\
    859-3:1988utf-16iso-8859-1cp819iso_8859-7chinesehebrewcsisolatin6csksc56011987is\
    o885910ibm819koreaniso-8859-5csisolatincyrilliccsiso88598iiso_8859-3koix-cp1255x\
    -cp1258gb2312iso-ir-149iso-8859-9l5x-cp1251csmacintoshiso-ir-138iso_8859-15csiso\
    latin3koi8-rx-cp1250x-cp1257x-cp1254iso-8859-14x-cp1256x-cp1253windows-1251csshi\
    ftjisiso_8859-6:1987iso8859-3windows-949csisolatinarabiciso885913l3csisolatin4ma\
    cintosheuc-krx-mac-romansjisiso8859-14hz-gb-2312csunicodecn-big5cseuckrcyrillicc\
    siso88596iiso_8859-6iso-8859-11iso_8859-2:1987iso-8859-4unicodefffelatin4ecma-11\
    8iso-ir-144windows-31jecma-114iso-ir-127latin1866cskoi8riso8859-2iso-ir-148windo\
    ws-1256csiso88596ewindows-1255latin6iso-8859-8iso_8859-2greek8csgb2312iso-8859-6\
    -eunicodegreekiso-ir-101iso8859-10csiso2022jpksc_5601macx-mac-ukrainianiso-8859-\
    6-ikoi8-uwindows-1252ksc5601iso_8859-5:1988gb_2312-80us-asciiiso_8859-9ks_c_5601\
    -1989iso-8859-15ks_c_5601-1987iso-2022-cn-extcsisolatinhebrewiso8859-13iso_8859-\
    1:1987iso-ir-58cp866iso-8859-3iso_8859-5iso-ir-109csiso2022krms932iso8859-5l2iso\
    885915x-gbkiso-ir-126csiso58gb231280csisolatin5utf8tis-620iso_8859-1windows-1253\
    csisolatin1windows-1257iso-8859-7utf-8iso885914iso-ir-100koi8_rasmo-708iso8859-4\
    iso-ir-157logicalcseucpkdfmtjapaneseiso-2022-krunicode11utf8koi8x-unicode20utf8w\
    indows-874iso88597big5-hkscsiso88594l1cp1256unicode20utf8iso_8859-4:1988iso_8859\
    -8:1988cp1250cp1253iso-8859-16ucs-2x-user-definedcsisolatin9gbkcp1254iso8859-7is\
    o-8859-2iso_8859-8big5iso-ir-110windows-1258unicodefeffcp1255cp1257cp1258cp1251u\
    nicode-1-1-utf-8ms_kanjicp1252replacementibm866shift-jisiso-8859-6csiso88598eiso\
    88592csisolatin2x-sjiswindows-1254csibm866iso_8859-4elot_928x-euc-jpiso-8859-13i\
    so-8859-8-egb18030shift_jisiso88595iso88593iso88599ansi_x3.4-1968dos-874iso88598\
    iso_8859-9:1989visuall6x-mac-cyrilliciso88596csbig5iso8859-6windows-1250x-x-big5\
    iso88591\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
