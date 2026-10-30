//! One test per guard: each fails if its guard is removed or weakened.

use std::path::Path;
use std::process::Command;

use serde_json::json;
use tundish::config::Config;
use tundish::rules::{Consume, Rules, Share};
use tundish::{AckOutcome, PullOpts, Retention, Store};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();
    (dir, s)
}

fn put(s: &mut Store, q: &str, n: i64, now: i64) -> i64 {
    s.publish(
        q,
        None,
        &json!({"n": n, "even": n % 2 == 0}),
        now,
        Retention::default(),
    )
    .unwrap()
}

fn pull(s: &mut Store, sub: &str, who: &str, now: i64) -> String {
    let mut out = Vec::new();
    s.pull(sub, who, &PullOpts::at(now), &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn on_ack(q: &str) -> Rules {
    let mut r = Rules::new(q);
    r.consume = Consume::OnAck;
    r
}

fn pull_due(s: &mut Store, who: &str, opts: &PullOpts) -> String {
    let mut out = Vec::new();
    s.pull_due(who, opts, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn ids(text: &str) -> Vec<i64> {
    text.lines()
        .filter_map(|l| l.strip_prefix('[')?.split_once(']'))
        .filter_map(|(tag, _)| tag.rsplit_once('#')?.1.parse().ok())
        .collect()
}

#[test]
fn a_share_one_subscription_moved_to_a_queue_with_lower_ids_reads_it_all() {
    let (_d, mut s) = store();
    let b = vec![put(&mut s, "b", 1, 1), put(&mut s, "b", 2, 1)];
    for n in 0..3 {
        put(&mut s, "a", n, 1);
    }
    let mut r = Rules::new("a");
    r.share = Share::One;
    s.subscribe("work", "w", Some(r.clone()), 2).unwrap();
    assert_eq!(ids(&pull(&mut s, "work", "w", 3)).len(), 3);
    r.queue = "b".into();
    s.subscribe("work", "w", Some(r), 4).unwrap();
    assert_eq!(ids(&pull(&mut s, "work", "w", 5)), b);
}

#[test]
fn a_parked_item_does_not_hold_retention() {
    let (_d, mut s) = store();
    let mut r = on_ack("q");
    r.share = Share::One;
    r.lease = 1;
    r.max_attempts = 1;
    s.subscribe("work", "w", Some(r), 0).unwrap();
    put(&mut s, "q", 1, 0);
    pull(&mut s, "work", "w", 1);
    assert!(pull(&mut s, "work", "w", 5).contains("parked"));
    let later = vec![put(&mut s, "q", 2, 6), put(&mut s, "q", 3, 6)];
    pull(&mut s, "work", "w", 7);
    for id in later {
        s.ack(id, "w", 8).unwrap();
    }
    pull(&mut s, "work", "w", 9);
    let keep = Retention {
        max_items: Some(1),
        max_age: None,
    };
    s.publish("q", None, &json!({"n": 4}), 10, keep).unwrap();
    assert_eq!(s.count("q").unwrap(), 1, "the parked head held the floor");
}

#[test]
fn a_due_pull_reports_a_drop_even_with_no_items() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.filter = Some(".payload.even".into());
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    put(&mut s, "q", 0, 1);
    let aged = Retention {
        max_items: None,
        max_age: Some(10),
    };
    s.publish("q", None, &json!({"n": 1, "even": false}), 100, aged)
        .unwrap();
    let out = pull_due(&mut s, "a", &PullOpts::at(101));
    assert!(ids(&out).is_empty(), "{out}");
    assert!(out.contains("1 unread item in feed was dropped"), "{out}");
}

#[test]
fn rules_need_a_queue() {
    assert!(Rules::new("").check().is_err());
}

#[test]
fn a_full_scan_window_with_no_match_says_pull_again() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.filter = Some(".payload.n == -1".into());
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    for n in 0..1001 {
        put(&mut s, "q", n, 1);
    }
    let out = pull(&mut s, "feed", "a", 2);
    assert!(out.contains("more in feed: pull again"), "{out}");
}

#[test]
fn a_due_pull_stays_inside_its_byte_budget() {
    let (_d, mut s) = store();
    s.subscribe("a1", "z", Some(Rules::new("q1")), 0).unwrap();
    let mut r = Rules::new("q2");
    r.filter = Some(".payload.n | ascii_downcase".into());
    s.subscribe("b1", "z", Some(r), 0).unwrap();
    for n in 0..40 {
        put(&mut s, "q1", n, 1);
    }
    put(&mut s, "q2", 1, 1);
    let mut opts = PullOpts::at(2);
    opts.max_bytes = 200;
    let out = pull_due(&mut s, "z", &opts);
    assert!(out.len() <= 200, "{} bytes: {out}", out.len());
}

#[test]
fn a_database_with_a_newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    drop(Store::open(&path).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    let err = Store::open(&path).err().expect("a newer schema opened");
    assert!(err.to_string().contains("schema 99"), "{err}");
}

#[test]
fn an_explicit_ack_subscription_must_be_held_on_the_item_queue() {
    let (_d, mut s) = store();
    s.subscribe("mine", "a", Some(on_ack("q")), 0).unwrap();
    s.subscribe("theirs", "b", Some(on_ack("q")), 0).unwrap();
    s.subscribe("elsewhere", "a", Some(on_ack("other")), 0)
        .unwrap();
    let id = put(&mut s, "q", 1, 1);
    assert!(s.ack_in(id, "a", Some("theirs"), 2).is_err());
    assert!(s.ack_in(id, "a", Some("elsewhere"), 2).is_err());
    assert_eq!(
        s.ack_in(id, "a", Some("mine"), 2).unwrap(),
        AckOutcome::Acked
    );
}

#[test]
fn an_expired_lease_no_longer_refuses_another_ack() {
    let (_d, mut s) = store();
    let mut r = on_ack("q");
    r.share = Share::One;
    r.lease = 10;
    s.subscribe("work", "a", Some(r), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let id = put(&mut s, "q", 1, 1);
    assert!(pull(&mut s, "work", "a", 1).contains(&format!("#{id}]")));
    assert!(matches!(
        s.ack(id, "b", 5).unwrap(),
        AckOutcome::LeasedBy { .. }
    ));
    assert_eq!(s.ack(id, "b", 20).unwrap(), AckOutcome::Acked);
}

#[test]
fn list_unacked_counts_acks_in_the_named_subscription_only() {
    let (_d, mut s) = store();
    s.subscribe("one", "a", Some(on_ack("q")), 0).unwrap();
    s.subscribe("two", "b", Some(on_ack("q")), 0).unwrap();
    let id = put(&mut s, "q", 1, 1);
    s.ack(id, "a", 2).unwrap();
    let ids = |s: &Store, sub| -> Vec<i64> {
        s.list("q", true, Some(sub), 100)
            .unwrap()
            .iter()
            .map(|i| i.id)
            .collect()
    };
    assert!(ids(&s, "one").is_empty());
    assert_eq!(ids(&s, "two"), vec![id]);
}

#[test]
fn unsubscribe_removes_the_binding_once() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    assert!(s.unsubscribe("feed", "a").unwrap());
    assert!(!s.unsubscribe("feed", "a").unwrap());
    assert!(
        s.pull("feed", "a", &PullOpts::at(1), &mut Vec::new())
            .is_err()
    );
}

#[test]
fn the_unacked_count_follows_the_filter() {
    let (_d, mut s) = store();
    let mut r = on_ack("q");
    r.filter = Some(".payload.even".into());
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    for n in 0..4 {
        put(&mut s, "q", n, 1);
    }
    pull(&mut s, "feed", "a", 2);
    let out = pull(&mut s, "feed", "a", 3);
    assert!(out.contains("2 unacked in feed"), "{out}");
}

#[test]
fn the_drop_count_follows_the_filter_and_is_reported_once() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.filter = Some(".payload.even".into());
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    for n in 0..4 {
        put(&mut s, "q", n, 1);
    }
    let aged = Retention {
        max_items: None,
        max_age: Some(10),
    };
    s.publish("q", None, &json!({"n": 10, "even": true}), 100, aged)
        .unwrap();
    let out = pull(&mut s, "feed", "a", 101);
    assert!(out.contains("2 unread items in feed were dropped"), "{out}");
    s.publish("q", None, &json!({"n": 12, "even": true}), 102, aged)
        .unwrap();
    let again = pull(&mut s, "feed", "a", 103);
    assert!(!again.contains("dropped"), "reported twice: {again}");
    let aged_one = Retention {
        max_items: None,
        max_age: Some(10),
    };
    put(&mut s, "q", 20, 104);
    s.publish("q", None, &json!({"n": 22, "even": true}), 200, aged_one)
        .unwrap();
    let one = pull(&mut s, "feed", "a", 201);
    assert!(one.contains("1 unread item in feed was dropped"), "{one}");
}

#[test]
fn a_byte_budget_below_the_minimum_is_refused() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let mut opts = PullOpts::at(1);
    opts.max_bytes = 127;
    assert!(s.pull("feed", "a", &opts, &mut Vec::new()).is_err());
    assert!(s.pull_due("a", &opts, &mut Vec::new()).is_err());
    opts.max_bytes = 128;
    assert!(s.pull("feed", "a", &opts, &mut Vec::new()).is_ok());
}

#[test]
fn empty_names_are_refused() {
    let (_d, mut s) = store();
    assert!(
        s.publish("", None, &json!({}), 1, Retention::default())
            .is_err()
    );
    assert!(s.subscribe("", "a", Some(Rules::new("q")), 0).is_err());
    assert!(s.subscribe("feed", "", Some(Rules::new("q")), 0).is_err());
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let id = put(&mut s, "q", 1, 1);
    assert!(s.ack_in(id, "", None, 2).is_err());
}

#[test]
fn lease_and_attempts_must_be_positive() {
    for (lease, attempts) in [(0, 3), (-1, 3), (600, 0), (600, -1)] {
        let mut r = Rules::new("q");
        r.lease = lease;
        r.max_attempts = attempts;
        assert!(r.check().is_err(), "lease {lease}, attempts {attempts}");
    }
}

fn config(dir: &Path, text: &str) -> tundish::Result<Config> {
    let path = dir.join("c.yml");
    std::fs::write(&path, text).unwrap();
    Config::load(Some(&path))
}

#[test]
fn queue_retention_comes_from_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(
        dir.path(),
        "format: 1\nqueues:\n  slack:\n    max_items: 5\n    max_age: 10m\n",
    )
    .unwrap();
    assert_eq!(
        c.retention("slack"),
        Retention {
            max_items: Some(5),
            max_age: Some(600)
        }
    );
    assert_ne!(c.retention("other"), c.retention("slack"));
    assert!(config(dir.path(), "format: 1\nqueues:\n  q:\n    max_items: 0\n").is_err());
}

fn cli(dir: &Path, args: &[&str]) -> std::process::Output {
    let config = dir.join("c.yml");
    std::fs::write(&config, "format: 1\n").unwrap();
    Command::new(env!("CARGO_BIN_EXE_tundish"))
        .arg("--db")
        .arg(dir.join("t.db"))
        .arg("--config")
        .arg(config)
        .args(args)
        .env_remove("GAFF_TIMEOUT_MS")
        .output()
        .unwrap()
}

#[test]
fn the_cli_refuses_a_missing_item_an_absent_binding_and_bad_stored_rules() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    assert!(
        cli(d, &["subscribe", "feed", "--as", "a", "--queue", "q"])
            .status
            .success()
    );
    assert_eq!(cli(d, &["ack", "7", "--as", "a"]).status.code(), Some(2));
    assert_eq!(
        cli(d, &["unsubscribe", "feed", "--as", "zed"])
            .status
            .code(),
        Some(2)
    );
    assert!(cli(d, &["subscribe", "feed", "--as", "b"]).status.success());
    let gone = cli(d, &["unsubscribe", "feed", "--as", "b"]);
    assert!(gone.status.success());
    assert_eq!(
        String::from_utf8_lossy(&gone.stdout),
        "unbound b from feed; its read position was removed\n"
    );
    assert!(cli(d, &["check"]).status.success());
    rusqlite::Connection::open(d.join("t.db"))
        .unwrap()
        .execute(
            "UPDATE subscriptions SET rules = ?1 WHERE name = 'feed'",
            [r#"{"queue":"q","select":"latest"}"#],
        )
        .unwrap();
    let out = cli(d, &["check"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("feed"));
}
