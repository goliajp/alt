//! What the block size of a version 2 log costs: how long the file gets
//! and how much of it an open reads.
//!
//! `oplog_blocks fill <dir> <block exponent> <stagings> <workspaces> [full]`
//! writes a log as that many `alt add` would: an op of some 320 bytes and
//! a receipt each, none synced; with `full`, more of them until the last
//! block is nearly full. `oplog_blocks open <dir>` opens it as a command
//! that reads does. `oplog_blocks nothing` does neither, for the cost of
//! a process.
//!
//! `oplog_blocks tables <dir> <block exponent> <workspaces> <commits>
//! <one|all>` writes a log in which that many workspaces each staged
//! once, none synced, and then that many commits were made and synced:
//! all by one workspace, or by all of them in turn.

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

/// A commit of `workspace`: a ref transaction of some 300 bytes and its
/// receipt, synced.
fn commit(log: &mut OpLog, n: u64, workspace: u64) {
    let meta = Meta {
        workspace: Some((workspace + 1).to_le_bytes()),
        undoable: true,
        linked: true,
        index_after: Some([n as u8; 20]),
        request: Some((n as u128 + 1).to_le_bytes()),
        ..Meta::default()
    };
    let actor = "human:root;user:root;verb:commit";
    log.append_with(actor, n, &[n as u8; 120], &meta).unwrap();
    let mut receipt = Receipt {
        request: (n as u128 + 1).to_le_bytes(),
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
    receipt.answer(b"[main 0123abc] a commit\n");
    log.append_receipt(&receipt).unwrap();
    log.sync().unwrap();
}

fn tables(dir: &Path, exp: u8, workspaces: u64, commits: u64, by_all: bool) {
    alt_oplog::v2::create_with(dir, exp).unwrap();
    let mut log = OpLog::open(dir).unwrap();
    {
        let _unsynced = alt_os::durability::leave_unsynced(true);
        for workspace in 0..workspaces {
            let meta = Meta {
                workspace: Some((workspace + 1).to_le_bytes()),
                index_after: Some([workspace as u8; 20]),
                ..Meta::default()
            };
            let actor = "human:root;user:root;verb:add";
            log.append_with(actor, workspace, &[1; 122], &meta).unwrap();
        }
    }
    let staged = std::fs::metadata(dir.join("log")).unwrap().len();
    for n in 0..commits {
        commit(&mut log, n, if by_all { n % workspaces } else { 0 });
    }
    drop(log);
    let len = std::fs::metadata(dir.join("log")).unwrap().len();
    let log = OpLog::open_read_only(dir, None).unwrap();
    let (ops, read) = (log.len(), log.bytes_read());
    log.verify().unwrap();
    println!(
        "workspaces {workspaces} commits {commits} by {} bytes {len} after the stagings {staged} \
         for each commit {} an open reads {read} ops {ops}",
        if by_all { "all" } else { "one" },
        (len - staged) / commits,
    );
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
        "tables" => {
            let number = |at: usize| args[at].parse::<u64>().unwrap();
            let by_all = args[5] == "all";
            tables(
                Path::new(&args[1]),
                number(2) as u8,
                number(3),
                number(4),
                by_all,
            );
        }
        "open" => {
            let log = OpLog::open_read_only(Path::new(&args[1]), None).unwrap();
            println!("ops {} read {}", log.len(), log.bytes_read());
        }
        _ => println!("nothing"),
    }
}
