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
fn a_move_resets_acks_leases_and_pending_drops() {
    let (_d, mut s) = store();
    let a = put(&mut s, "a", 1, 1);
    put(&mut s, "b", 2, 1);
    let mut one = on_ack("a");
    one.share = Share::One;
    s.subscribe("work", "w", Some(one.clone()), 2).unwrap();
    assert_eq!(ids(&pull(&mut s, "work", "w", 3)), vec![a], "leased");
    let mut read = Rules::new("a");
    read.share = Share::One;
    s.subscribe("other", "w", Some(read.clone()), 2).unwrap();
    assert_eq!(
        ids(&pull(&mut s, "other", "w", 3)),
        vec![a],
        "acked on read"
    );
    for sub in ["work", "other"] {
        let mut moved = if sub == "work" {
            one.clone()
        } else {
            read.clone()
        };
        moved.queue = "b".into();
        s.subscribe(sub, "w", Some(moved.clone()), 4).unwrap();
        moved.queue = "a".into();
        s.subscribe(sub, "w", Some(moved), 5).unwrap();
        assert_eq!(ids(&pull(&mut s, sub, "w", 6)), vec![a], "{sub} reset");
    }

    let mut feed = Rules::new("a");
    feed.start = tundish::rules::Start::All;
    s.subscribe("feed", "f", Some(feed.clone()), 7).unwrap();
    let aged = Retention {
        max_items: None,
        max_age: Some(10),
    };
    s.publish("a", None, &json!({"n": 9}), 100, aged).unwrap();
    feed.queue = "b".into();
    s.subscribe("feed", "f", Some(feed), 101).unwrap();
    let out = pull(&mut s, "feed", "f", 102);
    assert!(
        out.contains(" dropped=0 "),
        "a drop from the old queue: {out}"
    );
}

#[test]
fn acked_items_are_not_counted_as_drops() {
    let (_d, mut s) = store();
    let mut r = on_ack("q");
    r.share = Share::One;
    r.limit = Some(1);
    s.subscribe("work", "a", Some(r), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    put(&mut s, "q", 1, 1);
    put(&mut s, "q", 2, 1);
    pull(&mut s, "work", "a", 2);
    let second = ids(&pull(&mut s, "work", "b", 2));
    s.ack(second[0], "b", 3).unwrap();
    let aged = Retention {
        max_items: None,
        max_age: Some(10),
    };
    s.publish("q", None, &json!({"n": 3}), 100, aged).unwrap();
    let out = pull(&mut s, "work", "a", 101);
    assert!(out.contains(" dropped=1 "), "{out}");
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
    assert!(pull(&mut s, "work", "w", 5).contains(" parked=1 "));
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
    assert!(out.contains(" dropped=1 "), "{out}");
}

#[test]
fn a_share_all_subscription_moved_to_a_queue_with_lower_ids_reads_it_all() {
    let (_d, mut s) = store();
    let b = put(&mut s, "b", 1, 1);
    put(&mut s, "a", 2, 1);
    put(&mut s, "a", 3, 1);
    s.subscribe("feed", "w", Some(Rules::new("a")), 2).unwrap();
    assert_eq!(ids(&pull(&mut s, "feed", "w", 3)).len(), 2);
    s.subscribe("feed", "w", Some(Rules::new("b")), 4).unwrap();
    assert_eq!(ids(&pull(&mut s, "feed", "w", 5)), vec![b]);
}

#[test]
fn an_expired_lease_under_max_attempts_is_retried_after_a_pull_with_no_room() {
    let (_d, mut s) = store();
    // `big` fills a due pull, so `work` delivers nothing in it.
    s.subscribe("big", "z", Some(Rules::new("big")), 0).unwrap();
    let mut r = on_ack("q");
    r.share = Share::One;
    r.lease = 10;
    r.max_attempts = 3;
    s.subscribe("work", "z", Some(r), 0).unwrap();
    let item = s
        .publish(
            "q",
            None,
            &json!({"pad": "x".repeat(200)}),
            1,
            Retention::default(),
        )
        .unwrap();
    assert_eq!(ids(&pull(&mut s, "work", "z", 1)), vec![item], "leased");
    s.publish(
        "big",
        None,
        &json!({"pad": "x".repeat(570)}),
        2,
        Retention::default(),
    )
    .unwrap();
    let due = pull_due(&mut s, "z", &PullOpts::at(20));
    assert!(
        due.contains("subscription=work subscriber=z delivered=0 more=1 "),
        "work had no room: {due}"
    );
    assert_eq!(
        ids(&pull(&mut s, "work", "z", 21)),
        vec![item],
        "the expired lease returns the item, under max attempts"
    );
}

// share one selection: new rows by a forward scan, retries by the
// lease table.

fn one(q: &str) -> Rules {
    let mut r = on_ack(q);
    r.share = Share::One;
    r
}

/// One count from the status line of `text`, or 0 when nothing printed.
fn status_count(text: &str, key: &str) -> i64 {
    text.lines()
        .find(|l| l.starts_with("tundish: "))
        .and_then(|l| {
            l.split(' ')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.parse().unwrap())
        })
        .unwrap_or(0)
}

#[test]
fn a_held_lease_does_not_hide_free_items_past_the_scan_window() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.lease = 100_000;
    r.limit = Some(1);
    s.subscribe("work", "a", Some(r), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let all: Vec<i64> = (0..1005).map(|n| put(&mut s, "q", n, 1)).collect();
    assert_eq!(
        ids(&pull(&mut s, "work", "a", 2)),
        vec![all[0]],
        "a holds it"
    );
    for id in &all[1..1000] {
        assert_eq!(s.ack(*id, "b", 3).unwrap(), AckOutcome::Acked);
    }
    let mut got = Vec::new();
    for now in 4..20 {
        let out = pull(&mut s, "work", "b", now);
        if out.is_empty() {
            break;
        }
        got.extend(ids(&out));
    }
    assert_eq!(got, all[1000..].to_vec(), "the free items after 1000");
}

#[test]
fn a_parked_item_is_reported_once_and_a_higher_max_attempts_does_not_revive_it() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.lease = 10;
    r.max_attempts = 1;
    s.subscribe("work", "a", Some(r.clone()), 0).unwrap();
    let item = put(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), vec![item]);
    let parked = pull(&mut s, "work", "a", 20);
    assert_eq!(status_count(&parked, "parked"), 1, "{parked}");
    assert_eq!(pull(&mut s, "work", "a", 21), "", "reported once");
    r.max_attempts = 5;
    s.subscribe("work", "a", Some(r), 22).unwrap();
    assert_eq!(pull(&mut s, "work", "a", 30), "", "parked stays parked");
    let next = put(&mut s, "q", 2, 31);
    let out = pull(&mut s, "work", "a", 32);
    assert_eq!(ids(&out), vec![next], "{out}");
    assert_eq!(status_count(&out, "parked"), 0, "no re-report: {out}");
    assert_eq!(s.status(33).unwrap()[0].parked, 1);
}

#[test]
fn a_retry_is_served_even_when_a_new_filter_no_longer_matches_it() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.lease = 10;
    r.filter = Some(".payload.even".into());
    s.subscribe("work", "a", Some(r.clone()), 0).unwrap();
    let item = put(&mut s, "q", 0, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), vec![item]);
    r.filter = Some(".payload.even | not".into());
    s.subscribe("work", "a", Some(r), 3).unwrap();
    assert_eq!(ids(&pull(&mut s, "work", "a", 20)), vec![item], "retried");
    assert_eq!(s.ack(item, "a", 21).unwrap(), AckOutcome::Acked);
    assert_eq!(pull(&mut s, "work", "a", 22), "");
    assert_eq!(s.status(22).unwrap()[0].unread, 0, "not pinned");
    let odd = put(&mut s, "q", 1, 23);
    assert_eq!(ids(&pull(&mut s, "work", "a", 24)), vec![odd]);
}

#[test]
fn a_filter_that_fails_on_a_retry_is_not_counted_as_an_error() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.lease = 10;
    r.max_attempts = 3;
    r.filter = Some(".payload.x | length > 0".into());
    s.subscribe("work", "a", Some(r.clone()), 0).unwrap();
    let item = s
        .publish("q", None, &json!({"x": "y"}), 1, Retention::default())
        .unwrap();
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), vec![item]);
    r.filter = Some(".payload.x + 1 > 0".into());
    s.subscribe("work", "a", Some(r), 3).unwrap();
    let mut errors = 0;
    let mut got = Vec::new();
    for now in [20, 40, 60, 80] {
        let out = pull(&mut s, "work", "a", now);
        errors += status_count(&out, "errors");
        got.extend(ids(&out));
    }
    assert_eq!(got, vec![item, item], "two retries, then parked");
    assert_eq!(errors, 0, "a retry does not run the filter");
}

#[test]
fn a_pre_acked_row_after_the_scan_is_not_delivered() {
    let (_d, mut s) = store();
    s.subscribe("work", "a", Some(one("q")), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let first = put(&mut s, "q", 1, 1);
    let second = put(&mut s, "q", 2, 1);
    assert_eq!(s.ack(second, "b", 2).unwrap(), AckOutcome::Acked);
    assert_eq!(ids(&pull(&mut s, "work", "a", 3)), vec![first]);
}

#[test]
fn a_held_item_is_not_given_to_a_second_taker_after_a_share_change_and_back() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.limit = Some(1);
    s.subscribe("work", "a", Some(r.clone()), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let held = put(&mut s, "q", 1, 1);
    let free = put(&mut s, "q", 2, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), vec![held]);
    let mut all = r.clone();
    all.share = Share::All;
    s.subscribe("work", "a", Some(all), 3).unwrap();
    s.subscribe("work", "a", Some(r), 4).unwrap();
    assert_eq!(ids(&pull(&mut s, "work", "b", 5)), vec![free]);
}

#[test]
fn a_share_change_to_all_clears_leases() {
    let (_d, mut s) = store();
    s.subscribe("work", "a", Some(one("q")), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let held = put(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), vec![held]);
    s.subscribe("work", "a", Some(on_ack("q")), 3).unwrap();
    assert_eq!(
        s.ack(held, "b", 4).unwrap(),
        AckOutcome::Acked,
        "a share: one lease means nothing under share: all"
    );
}

#[test]
fn a_max_age_trim_of_an_open_lease_counts_one_drop_once() {
    let (_d, mut s) = store();
    s.subscribe("work", "a", Some(one("q")), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let held = put(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), vec![held]);
    let aged = Retention {
        max_items: None,
        max_age: Some(10),
    };
    let fresh = s.publish("q", None, &json!({"n": 2}), 100, aged).unwrap();
    assert!(s.show(held).unwrap().is_none(), "trimmed");
    let out = pull(&mut s, "work", "b", 101);
    assert_eq!(ids(&out), vec![fresh], "{out}");
    assert_eq!(status_count(&out, "dropped"), 1, "{out}");
    let again = pull(&mut s, "work", "a", 102);
    assert_eq!(status_count(&again, "dropped"), 0, "once: {again}");
}

#[test]
fn an_open_lease_holds_the_retention_floor() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.limit = Some(1);
    s.subscribe("work", "a", Some(r), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let held = put(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), vec![held]);
    for n in 2..4 {
        put(&mut s, "q", n, 3);
        let id = ids(&pull(&mut s, "work", "b", 4))[0];
        s.ack(id, "b", 5).unwrap();
    }
    let keep = Retention {
        max_items: Some(1),
        max_age: None,
    };
    s.publish("q", None, &json!({"n": 4}), 6, keep).unwrap();
    assert!(
        s.show(held).unwrap().is_some(),
        "the open lease was trimmed"
    );
}

#[test]
fn the_floor_lets_a_parked_item_go_and_ignores_an_unbound_subscription() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.lease = 1;
    r.max_attempts = 1;
    s.subscribe("work", "w", Some(r), 0).unwrap();
    s.subscribe("gone", "g", Some(one("q")), 0).unwrap();
    let parked = put(&mut s, "q", 1, 0);
    assert_eq!(ids(&pull(&mut s, "work", "w", 1)), vec![parked]);
    assert_eq!(
        ids(&pull(&mut s, "gone", "g", 1)),
        vec![parked],
        "g's lease"
    );
    s.unsubscribe("gone", "g").unwrap();
    assert_eq!(status_count(&pull(&mut s, "work", "w", 5), "parked"), 1);
    let later = vec![put(&mut s, "q", 2, 6), put(&mut s, "q", 3, 6)];
    assert_eq!(ids(&pull(&mut s, "work", "w", 7)), later);
    for id in later {
        s.ack(id, "w", 8).unwrap();
    }
    let keep = Retention {
        max_items: Some(1),
        max_age: None,
    };
    s.publish("q", None, &json!({"n": 4}), 10, keep).unwrap();
    assert_eq!(s.count("q").unwrap(), 1);
}

#[test]
fn a_pull_on_the_second_the_last_lease_expires_parks_the_item() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.lease = 10;
    r.max_attempts = 1;
    s.subscribe("work", "a", Some(r), 0).unwrap();
    let item = put(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 100)), vec![item]);
    let out = pull(&mut s, "work", "a", 110);
    assert!(ids(&out).is_empty(), "redelivered past max_attempts: {out}");
    assert_eq!(status_count(&out, "parked"), 1, "{out}");
    assert_eq!(s.status(110).unwrap()[0].parked, 1);
}

#[test]
fn a_pull_on_the_second_a_lease_expires_redelivers_the_item() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.lease = 10;
    r.max_attempts = 2;
    s.subscribe("work", "a", Some(r), 0).unwrap();
    let item = put(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 100)), vec![item]);
    assert_eq!(ids(&pull(&mut s, "work", "a", 110)), vec![item]);
}

#[test]
fn rules_need_a_queue() {
    assert!(Rules::new("").check().is_err());
}

#[test]
fn retries_come_first_within_the_limit_and_say_when_more_remain() {
    let (_d, mut s) = store();
    let mut r = one("q");
    r.lease = 10;
    r.limit = Some(1);
    s.subscribe("work", "a", Some(r), 0).unwrap();
    let first = put(&mut s, "q", 1, 1);
    let second = put(&mut s, "q", 2, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), vec![first]);
    assert_eq!(ids(&pull(&mut s, "work", "a", 3)), vec![second]);
    let st = &s.status(20).unwrap()[0];
    assert_eq!((st.cursor, st.unread), (second, 2), "two retries: {st:?}");
    let out = pull(&mut s, "work", "a", 20);
    assert_eq!(ids(&out), vec![first], "oldest retry first: {out}");
    assert_eq!(status_count(&out, "more"), 1, "a retry remains: {out}");
    let fresh = put(&mut s, "q", 3, 20);
    let out = pull(&mut s, "work", "a", 21);
    assert_eq!(ids(&out), vec![second], "{out}");
    assert_eq!(status_count(&out, "more"), 1, "the new row waits: {out}");
    assert_eq!(ids(&pull(&mut s, "work", "a", 22)), vec![fresh]);
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
    assert!(
        out.contains(" delivered=0 more=1 "),
        "the agent learns to pull again: {out}"
    );
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
    opts.max_bytes = tundish::store::MIN_BYTES;
    let out = pull_due(&mut s, "z", &opts);
    assert!(
        out.len() <= tundish::store::MIN_BYTES,
        "{} bytes: {out}",
        out.len()
    );
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
    put(&mut s, "q", 4, 3);
    let out = pull(&mut s, "feed", "a", 4);
    assert!(out.contains(" unacked=2 "), "{out}");
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
    assert!(out.contains(" dropped=2 "), "{out}");
    s.publish("q", None, &json!({"n": 12, "even": true}), 102, aged)
        .unwrap();
    let again = pull(&mut s, "feed", "a", 103);
    assert!(again.contains(" dropped=0 "), "reported twice: {again}");
    let aged_one = Retention {
        max_items: None,
        max_age: Some(10),
    };
    put(&mut s, "q", 20, 104);
    s.publish("q", None, &json!({"n": 22, "even": true}), 200, aged_one)
        .unwrap();
    let one = pull(&mut s, "feed", "a", 201);
    assert!(one.contains(" dropped=1 "), "{one}");
}

#[test]
fn a_byte_budget_below_the_minimum_is_refused() {
    use tundish::store::MIN_BYTES;
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let mut opts = PullOpts::at(1);
    opts.max_bytes = MIN_BYTES - 1;
    assert!(s.pull("feed", "a", &opts, &mut Vec::new()).is_err());
    assert!(s.pull_due("a", &opts, &mut Vec::new()).is_err());
    opts.max_bytes = MIN_BYTES;
    assert!(s.pull("feed", "a", &opts, &mut Vec::new()).is_ok());
    assert!(s.pull_due("a", &opts, &mut Vec::new()).is_ok());
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
    let absent = cli(d, &["unsubscribe", "feed", "--as", "zed"]);
    assert_eq!(absent.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&absent.stderr).contains("zed is not bound to feed"),
        "{}",
        String::from_utf8_lossy(&absent.stderr)
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
