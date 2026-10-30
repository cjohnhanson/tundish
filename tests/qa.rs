//! Regressions from the QA review, one test per finding.

use serde_json::json;
use tundish::rules::{Consume, Rules, Select, Share};
use tundish::{AckOutcome, PullOpts, Retention, Store};

#[test]
fn a_named_config_that_does_not_exist_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let typo = dir.path().join("confg.yml");
    let err = tundish::config::Config::load(Some(&typo)).unwrap_err();
    assert!(err.to_string().contains("confg.yml"), "{err}");
    assert!(
        tundish::config::Config::load(None).is_ok(),
        "the fixed path may be absent"
    );
}

#[test]
fn subscribe_reports_what_it_changed() {
    use tundish::store::Change;
    let (_d, mut s) = store();
    let o = s.subscribe("s", "r", Some(Rules::new("q")), 0).unwrap();
    assert_eq!((o.change, o.bound), (Change::Created, true));
    let o = s.subscribe("s", "r", Some(Rules::new("q")), 0).unwrap();
    assert_eq!((o.change, o.bound), (Change::Replaced, false));
    let o = s.subscribe("s", "r", Some(Rules::new("other")), 0).unwrap();
    assert_eq!(
        (o.change, o.bound),
        (
            Change::Moved {
                from: "q".to_string()
            },
            false
        )
    );
    let o = s.subscribe("s", "r2", None, 0).unwrap();
    assert_eq!((o.change, o.bound), (Change::None, true));
    assert_eq!(o.rules.queue, "other");
}

#[test]
fn a_due_pull_for_a_subscriber_with_no_subscriptions_is_an_error() {
    let (_d, mut s) = store();
    s.subscribe("s", "r", Some(Rules::new("q")), 0).unwrap();
    let err = s
        .pull_due("rr", &PullOpts::at(1), &mut Vec::new())
        .unwrap_err();
    assert!(err.to_string().contains("--as rr"), "{err}");
    assert!(
        s.pull_due("r", &PullOpts::at(1), &mut Vec::new()).is_ok(),
        "a bound subscriber with nothing due is fine"
    );
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();
    (dir, s)
}

fn pull_opts(s: &mut Store, sub: &str, who: &str, opts: &PullOpts) -> String {
    let mut out = Vec::new();
    s.pull(sub, who, opts, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn pull(s: &mut Store, sub: &str, who: &str, now: i64) -> String {
    pull_opts(s, sub, who, &PullOpts::at(now))
}

fn ids(text: &str) -> Vec<i64> {
    text.lines()
        .filter_map(|l| l.strip_prefix('[')?.split_once(']'))
        .filter_map(|(tag, _)| tag.rsplit_once('#')?.1.parse().ok())
        .collect()
}

fn rules(q: &str, f: impl FnOnce(&mut Rules)) -> Rules {
    let mut r = Rules::new(q);
    f(&mut r);
    r
}

fn put(s: &mut Store, q: &str, v: serde_json::Value, now: i64) -> i64 {
    s.publish(q, None, &v, now, Retention::default()).unwrap()
}

// B1
#[test]
fn each_subscription_keeps_its_own_claims() {
    let (_d, mut s) = store();
    s.subscribe(
        "reviewers",
        "r1",
        Some(rules("gh", |r| r.share = Share::One)),
        0,
    )
    .unwrap();
    s.subscribe(
        "deployers",
        "d1",
        Some(rules("gh", |r| r.share = Share::One)),
        0,
    )
    .unwrap();
    let id = put(&mut s, "gh", json!({"n": 1}), 1);
    assert_eq!(ids(&pull(&mut s, "reviewers", "r1", 2)), vec![id]);
    assert_eq!(ids(&pull(&mut s, "deployers", "d1", 2)), vec![id]);
}

#[test]
fn an_ack_in_one_subscription_does_not_hide_the_item_from_another() {
    let (_d, mut s) = store();
    s.subscribe(
        "notify",
        "n",
        Some(rules("q", |r| r.consume = Consume::OnAck)),
        0,
    )
    .unwrap();
    s.subscribe("work", "w", Some(rules("q", |r| r.share = Share::One)), 0)
        .unwrap();
    let id = put(&mut s, "q", json!({}), 1);
    assert_eq!(ids(&pull(&mut s, "notify", "n", 2)), vec![id]);
    assert_eq!(s.ack(id, "n", 3).unwrap(), AckOutcome::Acked);
    assert_eq!(ids(&pull(&mut s, "work", "w", 4)), vec![id]);
}

#[test]
fn an_ack_needs_a_subscription_on_the_items_queue() {
    let (_d, mut s) = store();
    let id = put(&mut s, "q", json!({}), 1);
    assert!(s.ack(id, "nobody", 2).is_err());
}

#[test]
fn an_ack_names_the_subscription_when_the_subscriber_has_two() {
    let (_d, mut s) = store();
    s.subscribe(
        "one",
        "a",
        Some(rules("q", |r| r.consume = Consume::OnAck)),
        0,
    )
    .unwrap();
    s.subscribe(
        "two",
        "a",
        Some(rules("q", |r| r.consume = Consume::OnAck)),
        0,
    )
    .unwrap();
    let id = put(&mut s, "q", json!({}), 1);
    assert!(s.ack(id, "a", 2).is_err(), "ambiguous");
    assert_eq!(
        s.ack_in(id, "a", Some("two"), 2).unwrap(),
        AckOutcome::Acked
    );
    let out = pull(&mut s, "one", "a", 3);
    assert!(out.contains("--subscription one"), "{out}");
}

// M1
#[test]
fn a_holder_does_not_see_its_own_live_lease_again() {
    let (_d, mut s) = store();
    let r = rules("q", |r| {
        r.share = Share::One;
        r.consume = Consume::OnAck;
    });
    s.subscribe("w", "a", Some(r), 0).unwrap();
    s.subscribe("w", "b", None, 0).unwrap();
    let id = put(&mut s, "q", json!({}), 1);
    assert_eq!(ids(&pull(&mut s, "w", "a", 1000)), vec![id]);
    assert_eq!(ids(&pull(&mut s, "w", "a", 1010)), Vec::<i64>::new());
    assert_eq!(ids(&pull(&mut s, "w", "a", 1020)), Vec::<i64>::new());
    let late = pull(&mut s, "w", "b", 2000);
    assert_eq!(
        ids(&late),
        vec![id],
        "one expired lease returns the item: {late}"
    );
}

// M2
#[test]
fn a_filter_error_skips_the_item_and_reports_it() {
    let (_d, mut s) = store();
    s.subscribe("good", "a", Some(Rules::new("g")), 0).unwrap();
    let bad = rules("b", |r| r.filter = Some(".payload.x | length > 1".into()));
    s.subscribe("bad", "a", Some(bad), 0).unwrap();
    let g = put(&mut s, "g", json!({"m": "hello"}), 1);
    put(&mut s, "b", json!(5), 1);
    let ok = put(&mut s, "b", json!({"x": "abc"}), 1);
    let mut out = Vec::new();
    s.pull_due("a", &PullOpts::at(2), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert_eq!(ids(&out), vec![ok, g], "{out}");
    assert!(out.contains("1 item in bad failed the filter"), "{out}");
    assert!(!out.contains("Str(["), "readable error: {out}");
    assert_eq!(
        pull(&mut s, "bad", "a", 3),
        "",
        "the bad item is past the cursor"
    );
}

#[test]
fn a_template_error_still_delivers_the_item_in_the_default_form() {
    let (_d, mut s) = store();
    let r = rules("q", |r| {
        r.template = Some(".payload.n | ascii_downcase".into())
    });
    s.subscribe("t", "a", Some(r), 0).unwrap();
    let id = put(&mut s, "q", json!({"n": 5}), 1);
    let out = pull(&mut s, "t", "a", 2);
    assert_eq!(ids(&out), vec![id], "{out}");
    assert!(out.contains("template error"), "{out}");
}

#[test]
fn a_template_with_no_output_prints_the_default_form() {
    let (_d, mut s) = store();
    let r = rules("q", |r| r.template = Some("empty".into()));
    s.subscribe("t", "a", Some(r), 0).unwrap();
    let id = put(&mut s, "q", json!({"n": 5}), 1);
    assert_eq!(ids(&pull(&mut s, "t", "a", 2)), vec![id]);
}

// M3
#[test]
fn latest_is_the_newest_in_the_queue_past_any_scan_window() {
    let (_d, mut s) = store();
    let r = rules("q", |r| {
        r.select = Select::Latest;
        r.limit = Some(2);
    });
    s.subscribe("lt", "a", Some(r), 0).unwrap();
    let all: Vec<i64> = (0..1205)
        .map(|i| put(&mut s, "q", json!({"n": i}), 1))
        .collect();
    assert_eq!(ids(&pull(&mut s, "lt", "a", 2)), all[1203..].to_vec());
    assert_eq!(pull(&mut s, "lt", "a", 3), "");
}

// M4
#[test]
fn share_one_retention_trims_delivered_items_and_reports_no_false_drops() {
    let (_d, mut s) = store();
    s.subscribe("w", "a", Some(rules("q", |r| r.share = Share::One)), 0)
        .unwrap();
    let keep = Retention {
        max_items: Some(3),
        max_age: None,
    };
    for i in 0..12 {
        s.publish("q", None, &json!({"n": i}), 1, keep).unwrap();
        pull(&mut s, "w", "a", 1);
    }
    s.publish("q", None, &json!({"n": 99}), 1, keep).unwrap();
    assert!(s.count("q").unwrap() <= 3, "{}", s.count("q").unwrap());
    pull(&mut s, "w", "a", 1);
    let aged = Retention {
        max_items: None,
        max_age: Some(10),
    };
    s.publish("q", None, &json!({"n": 100}), 100, aged).unwrap();
    let out = pull(&mut s, "w", "a", 101);
    assert!(
        !out.contains("dropped"),
        "every dropped item was delivered: {out}"
    );
    let st = s.status().unwrap();
    assert_eq!(st[0].unread, 0, "{st:?}");
}

// M5
#[test]
fn footers_count_against_the_byte_budget() {
    let (_d, mut s) = store();
    s.subscribe(
        "s1",
        "a",
        Some(rules("q1", |r| r.consume = Consume::OnAck)),
        0,
    )
    .unwrap();
    let r2 = rules("q2", |r| {
        r.consume = Consume::OnAck;
        r.share = Share::One;
    });
    s.subscribe("s2", "a", Some(r2), 0).unwrap();
    let big = "x".repeat(470);
    put(&mut s, "q1", json!({"t": big}), 1);
    put(&mut s, "q1", json!({"t": big}), 1);
    let third = put(&mut s, "q2", json!({"t": big}), 1);
    let mut out = Vec::new();
    s.pull_due("a", &PullOpts::at(2), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.len() <= 1024, "{} bytes:\n{out}", out.len());
    assert!(
        !out.contains(&format!("tundish show {third}")),
        "a later subscription's item is not cut: {out}"
    );
    let mut all = ids(&out);
    for now in 3..6 {
        let mut rest = Vec::new();
        s.pull_due("a", &PullOpts::at(now), &mut rest).unwrap();
        all.extend(ids(&String::from_utf8(rest).unwrap()));
    }
    assert!(
        all.contains(&third),
        "the q2 item arrives on a later pull: {all:?}"
    );
    assert_eq!(all.len(), 3, "nothing repeated: {all:?}");
}

#[test]
fn a_full_batch_says_more_remain() {
    let (_d, mut s) = store();
    s.subscribe("f", "a", Some(rules("q", |r| r.limit = Some(2))), 0)
        .unwrap();
    for i in 0..5 {
        put(&mut s, "q", json!({"n": i}), 1);
    }
    assert!(pull(&mut s, "f", "a", 2).contains("more in f"));
}

// minors
#[test]
fn rule_bounds() {
    assert!(rules("q", |r| r.limit = Some(0)).check().is_err());
    assert!(tundish::rules::duration("999999999999999999m").is_err());
}

#[test]
fn resubscribing_onto_another_queue_resets_the_cursor() {
    let (_d, mut s) = store();
    s.subscribe("s", "a", Some(Rules::new("a")), 0).unwrap();
    for i in 0..5 {
        put(&mut s, "a", json!({"n": i}), 1);
    }
    pull(&mut s, "s", "a", 2);
    let b1 = put(&mut s, "b", json!({}), 3);
    let b2 = put(&mut s, "b", json!({}), 3);
    s.subscribe("s", "a", Some(Rules::new("b")), 4).unwrap();
    assert_eq!(ids(&pull(&mut s, "s", "a", 5)), vec![b1, b2]);
}

#[test]
fn opening_an_existing_store_takes_no_write_lock() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("t.db");
    drop(Store::open(&path).unwrap());
    let holder = rusqlite::Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let s = Store::open_with(&path, std::time::Duration::from_millis(50)).unwrap();
    assert!(s.status().is_ok());
    holder.execute_batch("ROLLBACK").unwrap();
}
