//! Behaviour across separate processes, the way hooks run the binary.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn tundish(db: &Path, args: &[&str]) -> Command {
    let config = db.with_extension("yml");
    if !config.exists() {
        std::fs::write(&config, "format: 1\n").unwrap();
    }
    let mut c = Command::new(env!("CARGO_BIN_EXE_tundish"));
    c.arg("--db").arg(db).arg("--config").arg(config);
    c.args(args);
    c.env_remove("GAFF_TIMEOUT_MS");
    c
}

fn ok(db: &Path, args: &[&str]) -> String {
    let o = tundish(db, args).output().unwrap();
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn ids(text: &str) -> Vec<i64> {
    text.lines()
        .filter_map(|l| l.strip_prefix('[')?.split_once(']'))
        .filter_map(|(tag, _)| tag.rsplit_once('#')?.1.parse().ok())
        .collect()
}

#[test]
fn publish_reads_stdin_and_a_second_ack_exits_3() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("t.db");
    ok(
        &db,
        &[
            "subscribe",
            "work",
            "--as",
            "a",
            "--queue",
            "q",
            "--consume",
            "on-ack",
        ],
    );
    ok(&db, &["subscribe", "work", "--as", "b"]);
    let mut p = tundish(&db, &["publish", "q", "--key", "k1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    p.stdin
        .take()
        .unwrap()
        .write_all(br#"{"hello":"world"}"#)
        .unwrap();
    let id: i64 = String::from_utf8(p.wait_with_output().unwrap().stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let batch = ok(&db, &["pull", "work", "--as", "a"]);
    assert!(batch.contains("k1") && batch.contains("world"), "{batch}");
    assert_eq!(
        ok(&db, &["ack", &id.to_string(), "--as", "a"]).trim(),
        format!("acked {id}")
    );
    let second = tundish(&db, &["ack", &id.to_string(), "--as", "b"])
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&second.stdout).starts_with("already acked by a at "));
}

#[test]
fn twenty_pullers_take_each_item_exactly_once() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("t.db");
    ok(
        &db,
        &[
            "subscribe",
            "jobs",
            "--as",
            "w0",
            "--queue",
            "q",
            "--share",
            "one",
            "--limit",
            "3",
        ],
    );
    for i in 1..20 {
        ok(&db, &["subscribe", "jobs", "--as", &format!("w{i}")]);
    }
    let mut published = Vec::new();
    for n in 0..60 {
        published.push(
            ok(&db, &["publish", "q", &format!("{{\"n\":{n}}}")])
                .trim()
                .parse::<i64>()
                .unwrap(),
        );
    }
    let children: Vec<_> = (0..20)
        .map(|i| {
            tundish(&db, &["pull", "jobs", "--as", &format!("w{i}")])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outs: Vec<Output> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap())
        .collect();
    let mut got: Vec<i64> = Vec::new();
    for o in &outs {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        got.extend(ids(&String::from_utf8_lossy(&o.stdout)));
    }
    got.sort_unstable();
    assert_eq!(got, published, "every item once, none twice");
}

#[test]
fn ten_racing_acks_have_one_winner() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("t.db");
    ok(
        &db,
        &[
            "subscribe",
            "work",
            "--as",
            "agent0",
            "--queue",
            "q",
            "--consume",
            "on-ack",
        ],
    );
    for i in 1..10 {
        ok(&db, &["subscribe", "work", "--as", &format!("agent{i}")]);
    }
    let id = ok(&db, &["publish", "q", "{}"]);
    let id = id.trim();
    let children: Vec<_> = (0..10)
        .map(|i| {
            tundish(&db, &["ack", id, "--as", &format!("agent{i}")])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let codes: Vec<i32> = children
        .into_iter()
        .map(|c| c.wait_with_output().unwrap().status.code().unwrap())
        .collect();
    assert_eq!(codes.iter().filter(|c| **c == 0).count(), 1, "{codes:?}");
    assert_eq!(codes.iter().filter(|c| **c == 3).count(), 9, "{codes:?}");
}

#[test]
fn a_pull_blocked_past_the_gaff_deadline_fails_and_delivers_nothing() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("t.db");
    ok(&db, &["subscribe", "feed", "--as", "a", "--queue", "q"]);
    ok(&db, &["publish", "q", "{\"n\":1}"]);

    let holder = rusqlite::Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let blocked = tundish(&db, &["pull", "feed", "--as", "a"])
        .env("GAFF_TIMEOUT_MS", "300")
        .output()
        .unwrap();
    let waited = started.elapsed();
    holder.execute_batch("ROLLBACK").unwrap();
    assert!(
        waited < std::time::Duration::from_secs(2),
        "the pull waited {waited:?}, past the 300 ms deadline"
    );
    assert_eq!(
        blocked.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    assert!(blocked.stdout.is_empty());
    assert_eq!(
        ids(&ok(&db, &["pull", "feed", "--as", "a"])).len(),
        1,
        "still undelivered"
    );
}

#[test]
fn due_prints_nothing_until_the_window_passes() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("t.db");
    ok(
        &db,
        &[
            "--now",
            "1000",
            "subscribe",
            "digest",
            "--as",
            "a",
            "--queue",
            "q",
            "--every",
            "5m",
        ],
    );
    ok(&db, &["--now", "1001", "publish", "q", "{}"]);
    assert_eq!(
        ids(&ok(&db, &["--now", "1002", "pull", "--due", "--as", "a"])).len(),
        1
    );
    ok(&db, &["--now", "1003", "publish", "q", "{}"]);
    assert_eq!(
        ok(&db, &["--now", "1100", "pull", "--due", "--as", "a"]),
        ""
    );
    assert_eq!(
        ids(&ok(&db, &["--now", "1302", "pull", "--due", "--as", "a"])).len(),
        1
    );
}
