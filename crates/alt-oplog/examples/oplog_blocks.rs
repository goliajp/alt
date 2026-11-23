//! What the block size of a version 2 log costs: how long the file gets
//! and how much of it an open reads.
//!
//! `oplog_blocks fill <dir> <block exponent> <stagings> <workspaces> [full]`
//! writes a log as that many `alt add` would: an op of some 320 bytes and
//! a receipt each, none synced; with `full`, more of them until the last
//! block is nearly full. `oplog_blocks open <dir>` opens it as a command
//! that reads does. `oplog_blocks nothing` does neither, for the cost of
//! a process.

use std::path::Path;

use alt_oplog::v2::receipt::{Outcome, Receipt};
use alt_oplog::{Meta, OpLog};

fn stage(log: &mut OpLog, n: u64, workspaces: u64) {
    let workspace = [(n % workspaces) as u8 + 1; 8];
    let meta = Meta {
        workspace: Some(workspace),
        undoable: true,
        index_after: Some([n as u8; 20]),
        request: Some([n as u8; 16]),
        ..Meta::default()
    };
    let payload = [n as u8; 122];
    let actor = "human:root;user:root;verb:add";
    log.append_with(actor, n, &payload, &meta).unwrap();
    let mut receipt = Receipt {
        request: [n as u8; 16],
        binding: [1; 32],
        outcome: Outcome::Executed,
        exit: 0,
        first_op: log.anchor().unwrap().start,
        ops: 1,
        finished_ms: n,
        digest: [0; 32],
        len: 0,
        response: None,
    };
    receipt.answer(b"staged 1 path(s) in 2 ms\n");
    log.append_receipt(&receipt).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "fill" => {
            let dir = Path::new(&args[1]);
            let (exp, count) = (args[2].parse().unwrap(), args[3].parse::<u64>().unwrap());
            let workspaces = args[4].parse().unwrap();
            alt_oplog::v2::create_with(dir, exp).unwrap();
            let mut log = OpLog::open(dir).unwrap();
            log.append("init", 0, &[0; 300]).unwrap();
            log.sync().unwrap();
            let _unsynced = alt_os::durability::leave_unsynced(true);
            let mut n = 0;
            while n < count {
                stage(&mut log, n, workspaces);
                n += 1;
            }
            let block = 1u64 << exp;
            while args.get(5).is_some() && block - log.appended_len() % block > 1200 {
                stage(&mut log, n, workspaces);
                n += 1;
            }
            let len = std::fs::metadata(dir.join("log")).unwrap().len();
            println!("stagings {n} bytes {len} in the last block {}", len % block);
        }
        "open" => {
            let log = OpLog::open_read_only(Path::new(&args[1]), None).unwrap();
            println!("ops {} read {}", log.len(), log.bytes_read());
        }
        _ => println!("nothing"),
    }
}
