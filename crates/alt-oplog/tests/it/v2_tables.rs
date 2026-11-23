//! Version 2 logs whose checkpoints hold tables of every size, in blocks
//! of every size: what a writer laid is what a new handle reads.

use alt_oplog::{Meta, OpLog};

use super::v2_log::file_len;

/// A log with blocks of two to the power of `exp` bytes that holds an op
/// of each of `rows` workspaces and then ops of several lengths over
/// some three blocks more, none of it synced.
fn laid(dir: &std::path::Path, exp: u8, rows: usize) -> OpLog {
    alt_oplog::v2::create_with(dir, exp).unwrap();
    let mut log = OpLog::open(dir).unwrap();
    let _unsynced = alt_os::durability::leave_unsynced(true);
    for row in 0..rows as u64 {
        let meta = Meta {
            workspace: Some((row + 1).to_le_bytes()),
            index_after: Some([row as u8; 20]),
            ..Meta::default()
        };
        log.append_with("a", row, b"of a workspace", &meta).unwrap();
    }
    let enough = file_len(dir) + 3 * (1 << exp) + 3 * 60 * rows as u64;
    for n in 0..400usize {
        if file_len(dir) >= enough {
            break;
        }
        log.append("a", n as u64, &vec![n as u8; n * 37 % 300])
            .unwrap();
    }
    log
}

/// How many of the ops `log` holds have a frame whose header runs into
/// the next block.
fn headers_across_blocks(log: &OpLog, exp: u8) -> usize {
    let block = 1u64 << exp;
    let across = |op: &&alt_oplog::Op| block - op.frame.unwrap().at % block < 48;
    log.ops().unwrap().iter().filter(across).count()
}

#[test]
fn what_a_writer_laid_is_what_a_new_handle_reads_whatever_the_table_and_the_blocks() {
    let mut across = 0;
    for exp in 10..=14u8 {
        let a_block = (1usize << exp) / 60;
        let sizes = (0..=a_block + 3).chain(2 * a_block - 3..=2 * a_block + 3);
        for rows in sizes {
            let dir = alt_os::temp::tempdir().unwrap();
            let writer = laid(dir.path(), exp, rows);
            across += headers_across_blocks(&writer, exp);
            let said = format!("blocks of 2^{exp}, {rows} workspaces");
            for read_only in [false, true] {
                let reader = match read_only {
                    true => OpLog::open_read_only(dir.path(), None).unwrap(),
                    false => OpLog::open(dir.path()).unwrap(),
                };
                assert_eq!(reader.state(), writer.state(), "{said}");
                let read = reader.ops().unwrap_or_else(|e| panic!("{said}: {e}"));
                assert_eq!(read, writer.ops().unwrap(), "{said}");
                reader.verify().unwrap_or_else(|e| panic!("{said}: {e}"));
                let head = reader.get(&writer.head().unwrap()).unwrap();
                assert_eq!(head, writer.recent().last(), "{said}");
            }
        }
    }
    assert!(across > 0, "no frame began in the last bytes of a block");
}
