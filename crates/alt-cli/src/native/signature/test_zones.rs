//! The zones the tests read dates in, from files of their own: those
//! tzdata 2026f compiles, with every change listed to 2037 and the rule
//! for the years after in the last line. No test reads the zone database
//! of the machine it runs on, which may have another or none.

use std::sync::OnceLock;

use alt_os::temp::TempDir;
use alt_os::zone::Zone;

const FILES: [(&str, &[u8]); 5] = [
    (
        "Asia/Tokyo",
        include_bytes!("../../../../alt-os/src/zone/testdata/Asia_Tokyo"),
    ),
    (
        "America/New_York",
        include_bytes!("../../../../alt-os/src/zone/testdata/America_New_York"),
    ),
    (
        "Asia/Kathmandu",
        include_bytes!("../../../../alt-os/src/zone/testdata/Asia_Kathmandu"),
    ),
    (
        "Europe/Dublin",
        include_bytes!("../../../../alt-os/src/zone/testdata/Europe_Dublin"),
    ),
    (
        "Africa/Monrovia",
        include_bytes!("../../../../alt-os/src/zone/testdata/Africa_Monrovia"),
    ),
];

/// The files, written where a `TZ` can name them.
pub(super) struct Zones(TempDir);

impl Zones {
    pub(super) fn new() -> Self {
        let dir = alt_os::temp::tempdir().unwrap();
        for (name, file) in FILES {
            std::fs::write(dir.path().join(name.replace('/', "_")), file).unwrap();
        }
        Zones(dir)
    }

    /// What `TZ` is to be for the zone `tz` names by the name the zone
    /// database has for it, with or without a colon before it: the path
    /// of the file here. `UTC` is the `TZ` of nothing, and what names
    /// none of the files is itself.
    pub(super) fn tz(&self, tz: &str) -> String {
        let name = tz.strip_prefix(':').unwrap_or(tz);
        let colon = &tz[..tz.len() - name.len()];
        match FILES.iter().find(|(of, _)| *of == name) {
            Some(_) => {
                let file = self.0.path().join(name.replace('/', "_"));
                format!("{colon}{}", file.display())
            }
            None if name == "UTC" => String::new(),
            None => tz.to_owned(),
        }
    }
}

/// The zone `tz` is the `TZ` of, by [`Zones::tz`], where it names one
/// of the files or UTC.
pub(super) fn zone(tz: &str) -> Option<&'static Zone> {
    static READ: OnceLock<Vec<(&str, Zone)>> = OnceLock::new();
    let read = READ.get_or_init(|| {
        let zones = Zones::new();
        let names = FILES.iter().map(|(name, _)| *name).chain(["UTC"]);
        let read = names.map(|name| (name, Zone::of(Some(&zones.tz(name)))));
        read.collect()
    });
    let name = tz.strip_prefix(':').unwrap_or(tz);
    let found = read.iter().find(|(of, _)| *of == name);
    found.map(|(_, zone)| zone)
}
