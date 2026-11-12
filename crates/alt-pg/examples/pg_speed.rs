//! Times typical transactions against a server.
//!
//! usage: pg_speed <one|insert|many|tx5> <iterations> <rounds>
//! with the connection parameters in `ALT_SERVER_TEST_ADMIN_DSN`. Prints the
//! fastest round in nanoseconds per iteration.

use alt_pg::{Client, IsNull, Oid, ToSql, WRONG_TYPE, oid};
use std::time::Instant;

struct JsonText<'a>(&'a [u8]);

impl ToSql for JsonText<'_> {
    fn to_sql(&self, ty: Oid, out: &mut Vec<u8>) -> Result<IsNull, &'static str> {
        match ty {
            oid::JSONB => out.push(1),
            oid::JSON => {}
            _ => return Err(WRONG_TYPE),
        }
        out.extend_from_slice(self.0);
        Ok(IsNull::No)
    }
}

const SETUP: &str = "CREATE TEMP TABLE bench_kv(id bigint PRIMARY KEY, name text NOT NULL, doc jsonb NOT NULL, data bytea, flag boolean NOT NULL);
INSERT INTO bench_kv SELECT g, 'name-' || g, jsonb_build_object('n', g), decode(repeat('ab', 32), 'hex'), g % 2 = 0 FROM generate_series(1, 500) g;
CREATE TEMP TABLE bench_log(id bigint, doc jsonb NOT NULL, data bytea NOT NULL);";
const ONE: &str = "SELECT name, flag, id FROM bench_kv WHERE id=$1";
const INSERT: &str = "INSERT INTO bench_log(id, doc, data) VALUES($1,$2,$3)";
const MANY: &str = "SELECT id, name, flag, data FROM bench_kv ORDER BY id";
const UPDATE: &str = "UPDATE bench_kv SET flag = NOT flag WHERE id=$1";
const NAME: &str = "SELECT name FROM bench_kv WHERE id=$1";
const FLAG: &str = "SELECT flag FROM bench_kv WHERE id=$1";
const DOC: &[u8] = br#"{"kind":"push","refs":["refs/heads/main"],"actor":"owner","bytes":123456}"#;

fn run(client: &mut Client, workload: &str, iterations: u64, blob: &[u8]) -> u64 {
    let mut sum = 0u64;
    for index in 0..iterations {
        let id = (index % 500 + 1) as i64;
        match workload {
            "one" => {
                let row = client.query_one(ONE, &[&id]).unwrap();
                sum += row.get::<_, &str>(0).len() as u64
                    + u64::from(row.get::<_, bool>(1))
                    + row.get::<_, i64>(2) as u64;
            }
            "insert" => {
                sum += client
                    .execute(INSERT, &[&id, &JsonText(DOC), &blob])
                    .unwrap();
            }
            "many" => {
                for row in client.query(MANY, &[]).unwrap() {
                    sum += row.get::<_, i64>(0) as u64
                        + row.get::<_, &str>(1).len() as u64
                        + u64::from(row.get::<_, bool>(2))
                        + row.get::<_, Option<&[u8]>>(3).map_or(0, <[u8]>::len) as u64;
                }
            }
            "tx5" => {
                let mut tx = client.transaction().unwrap();
                let row = tx.query_one(ONE, &[&id]).unwrap();
                sum += row.get::<_, i64>(2) as u64;
                sum += tx.execute(INSERT, &[&id, &JsonText(DOC), &blob]).unwrap();
                sum += tx.execute(UPDATE, &[&id]).unwrap();
                sum += tx.query_one(NAME, &[&id]).unwrap().get::<_, &str>(0).len() as u64;
                sum += u64::from(tx.query_one(FLAG, &[&id]).unwrap().get::<_, bool>(0));
                tx.commit().unwrap();
            }
            other => panic!("unknown workload {other}"),
        }
    }
    sum
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, workload, iterations, rounds] = arguments.as_slice() else {
        panic!("usage: pg_speed <one|insert|many|tx5> <iterations> <rounds>");
    };
    let iterations: u64 = iterations.parse().unwrap();
    let rounds: u32 = rounds.parse().unwrap();
    let parameters = std::env::var("ALT_SERVER_TEST_ADMIN_DSN").unwrap();
    let mut client = Client::connect(&parameters).unwrap();
    client.batch_execute(SETUP).unwrap();
    // every statement is prepared before the timed part
    let mut tx = client.transaction().unwrap();
    for sql in [ONE, UPDATE, NAME, FLAG] {
        tx.query(sql, &[&1i64]).unwrap();
    }
    tx.query(MANY, &[]).unwrap();
    tx.execute(INSERT, &[&1i64, &JsonText(DOC), &&[0u8][..]])
        .unwrap();
    tx.rollback().unwrap();
    let blob = vec![0xa5u8; 1024];
    let mut best = u128::MAX;
    let mut sum = 0;
    for _ in 0..rounds {
        client.batch_execute("TRUNCATE bench_log").unwrap();
        let start = Instant::now();
        sum = run(&mut client, workload, iterations, &blob);
        if iterations > 0 {
            best = best.min(start.elapsed().as_nanos() / u128::from(iterations));
        }
    }
    println!("alt-pg {workload} iters={iterations} min_ns_per_op={best} checksum={sum}");
}
