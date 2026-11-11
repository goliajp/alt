//! Search parameters by strength and input size.

/// Largest window a decoder accepts without being told to.
pub(crate) const WINDOW_LOG_MAX: u32 = 27;

/// Largest hash table, as a log2 of its entries, grown to for a big prefix.
const TABLE_LOG_MAX: u32 = 22;

#[derive(Clone, Copy)]
pub(crate) struct Params {
    pub window_log: u32,
    /// table of 8-byte hashes; unused by the single-table matcher
    pub long_log: u32,
    /// table of `width`-byte hashes
    pub short_log: u32,
    pub width: u32,
    pub double: bool,
    /// candidates read from the tables are rarely unusable: the input is
    /// long, so the start of the frame, where the tables are empty, is a
    /// small part of it
    pub likely: bool,
}

impl Params {
    pub(crate) fn pick(level: i32, data_len: usize, prefix_len: usize) -> Self {
        let double = level == 0 || level >= 3;
        let total = data_len + prefix_len;
        let class = match total {
            0..=0x4000 => 3,
            0x4001..=0x2_0000 => 2,
            0x2_0001..=0x4_0000 => 1,
            _ => 0,
        };
        let (window_log, long_log, short_log, width) = if double {
            [
                (21, 17, 16, 5),
                (18, 16, 16, 4),
                (17, 16, 15, 5),
                (14, 15, 14, 4),
            ][class]
        } else {
            [
                (19, 0, 14, 7),
                (18, 0, 14, 6),
                (17, 0, 13, 6),
                (14, 0, 15, 5),
            ][class]
        };
        let total_log = total.max(2).next_power_of_two().ilog2();
        let mut params = Self {
            window_log,
            long_log: long_log.min(total_log.max(6) + 1),
            short_log: short_log.min(total_log.max(6) + 1),
            width,
            double,
            likely: false,
        };
        if prefix_len != 0 {
            // the window must reach all of the prefix, and the tables must
            // not forget most of it before the data arrives
            params.window_log = total_log.clamp(10, WINDOW_LOG_MAX);
            let grown = total_log.saturating_sub(2).min(TABLE_LOG_MAX);
            params.long_log = params.long_log.max(grown);
            params.short_log = params.short_log.max(grown.saturating_sub(double as u32));
        }
        params.likely = params.window_log >= 19;
        params
    }
}
