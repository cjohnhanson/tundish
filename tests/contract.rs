//! The pull output contract: one status line per printing subscription,
//! item lines an agent can ack, bounded names, guaranteed progress, and
//! one-time counters.

use std::path::Path;
use std::process::Command;

use serde_json::json;
use tundish::rules::{Consume, Rules, Share};
use tundish::store::MIN_BYTES;
use tundish::{PullOpts, Retention, Store};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();
    (dir, s)
}

fn put(s: &mut Store, q: &str, v: serde_json::Value, now: i64) -> i64 {
    s.publish(q, None, &v, now, Retention::default()).unwrap()
}

fn pull_at(s: &mut Store, sub: &str, who: &str, opts: &PullOpts) -> String {
    let mut out = Vec::new();
    s.pull(sub, who, opts, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn pull(s: &mut Store, sub: &str, who: &str, now: i64) -> String {
    pull_at(s, sub, who, &PullOpts::at(now))
}

fn pull_due(s: &mut Store, who: &str, opts: &PullOpts) -> String {
    let mut out = Vec::new();
    s.pull_due(who, opts, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn budget(now: i64, max_bytes: usize) -> PullOpts {
    let mut o = PullOpts::at(now);
    o.max_bytes = max_bytes;
    o
}

fn ids(text: &str) -> Vec<i64> {
    text.lines()
        .filter_map(|l| l.strip_prefix('[')?.split_once(']'))
        .filter_map(|(tag, _)| tag.rsplit_once('#')?.1.parse().ok())
        .collect()
}

/// The status lines of an output, in order.
fn status_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.starts_with("tundish: "))
        .collect()
}

/// One field of the status line for `sub`.
fn field(text: &str, sub: &str, key: &str) -> Option<String> {
    let line = status_lines(text)
        .into_iter()
        .find(|l| l.contains(&format!(" subscription={sub} ")))?;
    line.split(' ')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_owned())
}

fn count(text: &str, sub: &str, key: &str) -> i64 {
    field(text, sub, key)
        .unwrap_or_else(|| panic!("no status line for {sub}: {text}"))
        .parse()
        .unwrap()
}

fn on_ack(q: &str) -> Rules {
    let mut r = Rules::new(q);
    r.consume = Consume::OnAck;
    r
}

fn work_queue(q: &str) -> Rules {
    let mut r = on_ack(q);
    r.share = Share::One;
    r
}

// 0. The share one scan position.

#[test]
fn a_share_one_match_behind_a_full_window_of_rejected_rows_is_reached() {
    let (_d, mut s) = store();
    let mut r = work_queue("q");
    r.filter = Some(".payload.even".into());
    s.subscribe("work", "w", Some(r), 0).unwrap();
    for _ in 0..1000 {
        put(&mut s, "q", json!({"even": false}), 1);
    }
    let even = put(&mut s, "q", json!({"even": true}), 1);
    let mut got = Vec::new();
    for now in 2..6 {
        got.extend(ids(&pull(&mut s, "work", "w", now)));
    }
    assert_eq!(got, vec![even]);
}

#[test]
fn a_stop_inside_retries_keeps_the_scan_position() {
    let (_d, mut s) = store();
    let mut r = work_queue("q");
    r.filter = Some(".payload.even".into());
    r.limit = Some(1);
    r.lease = 10;
    s.subscribe("work", "w", Some(r.clone()), 0).unwrap();
    let a = put(&mut s, "q", json!({"even": true}), 1);
    put(&mut s, "q", json!({"even": false}), 1);
    let c = put(&mut s, "q", json!({"even": true}), 1);
    assert_eq!(ids(&pull(&mut s, "work", "w", 2)), vec![a]);
    assert_eq!(ids(&pull(&mut s, "work", "w", 3)), vec![c]);
    r.filter = Some(".payload.even | not".into());
    s.subscribe("work", "w", Some(r), 4).unwrap();
    assert_eq!(ids(&pull(&mut s, "work", "w", 30)), vec![a], "a retry");
    s.ack(c, "w", 30).unwrap();
    s.ack(a, "w", 30).unwrap();
    let after = pull(&mut s, "work", "w", 31);
    assert!(
        ids(&after).is_empty(),
        "an item behind the position came back: {after}"
    );
}

#[test]
fn more_is_set_when_retries_fill_their_bound() {
    let (_d, mut s) = store();
    let mut r = work_queue("q");
    r.lease = 10;
    s.subscribe("work", "w", Some(r), 0).unwrap();
    for n in 0..1001 {
        put(&mut s, "q", json!({"n": n}), 1);
    }
    let big = budget(2, 1_000_000);
    pull_at(&mut s, "work", "w", &big);
    pull_at(&mut s, "work", "w", &budget(3, 1_000_000));
    let retry = pull_at(&mut s, "work", "w", &budget(30, 1_000_000));
    assert_eq!(count(&retry, "work", "more"), 1, "1001 retries waiting");
}

// 1. The status line.

#[test]
fn a_pull_opens_with_one_status_line_in_the_fixed_format() {
    let (_d, mut s) = store();
    s.subscribe("feed", "reader", Some(Rules::new("q")), 0)
        .unwrap();
    let id = put(&mut s, "q", json!({"n": 1}), 1);
    assert_eq!(
        pull(&mut s, "feed", "reader", 2),
        format!(
            "tundish: subscription=feed subscriber=reader delivered=1 more=0 unacked=0 parked=0 errors=0 dropped=0 lease=0\n[q#{id}] {{\"n\":1}}\n"
        )
    );
}

#[test]
fn a_leased_delivery_reports_the_lease_in_seconds() {
    let (_d, mut s) = store();
    let mut r = on_ack("q");
    r.share = Share::One;
    r.lease = 90;
    s.subscribe("jobs", "w", Some(r), 0).unwrap();
    put(&mut s, "q", json!({}), 1);
    let out = pull(&mut s, "jobs", "w", 2);
    assert_eq!(count(&out, "jobs", "lease"), 90, "{out}");
}

#[test]
fn a_pull_with_nothing_to_report_prints_nothing() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(on_ack("q")), 0).unwrap();
    put(&mut s, "q", json!({}), 1);
    assert_eq!(ids(&pull(&mut s, "feed", "a", 2)).len(), 1);
    assert_eq!(
        pull(&mut s, "feed", "a", 3),
        "",
        "an unacked item alone is not news"
    );
}

#[test]
fn no_footer_text_survives() {
    let (_d, mut s) = store();
    let mut r = on_ack("q");
    r.limit = Some(1);
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    for n in 0..3 {
        put(&mut s, "q", json!({"n": n}), 1);
    }
    pull(&mut s, "feed", "a", 2);
    let out = pull(&mut s, "feed", "a", 3);
    assert_eq!(out.lines().count(), 2, "one status line, one item: {out}");
    assert_eq!(count(&out, "feed", "unacked"), 1, "{out}");
    assert_eq!(count(&out, "feed", "more"), 1, "{out}");
    for gone in ["ack:", "pull again", "unacked in", "tundish list"] {
        assert!(!out.contains(gone), "{gone:?} in {out}");
    }
}

#[test]
fn a_due_pull_prints_one_status_line_per_printing_subscription() {
    let (_d, mut s) = store();
    s.subscribe("one", "z", Some(Rules::new("q1")), 0).unwrap();
    s.subscribe("two", "z", Some(Rules::new("q2")), 0).unwrap();
    s.subscribe("idle", "z", Some(Rules::new("q3")), 0).unwrap();
    let a = put(&mut s, "q1", json!({}), 1);
    let b = put(&mut s, "q2", json!({}), 1);
    let out = pull_due(&mut s, "z", &PullOpts::at(2));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "{out}");
    assert!(lines[0].starts_with("tundish: subscription=one "), "{out}");
    assert_eq!(ids(lines[1]), vec![a]);
    assert!(lines[2].starts_with("tundish: subscription=two "), "{out}");
    assert_eq!(ids(lines[3]), vec![b]);
}

// 2. Item lines on an on-ack subscription.

#[test]
fn an_on_ack_template_without_the_id_still_shows_queue_and_id() {
    let (_d, mut s) = store();
    let mut r = on_ack("slack");
    r.template = Some(".payload.text".into());
    s.subscribe("inbox", "a", Some(r), 0).unwrap();
    let id = put(&mut s, "slack", json!({"text": "hi"}), 1);
    let out = pull(&mut s, "inbox", "a", 2);
    assert_eq!(
        out.lines().nth(1),
        Some(format!("[slack#{id}] hi").as_str())
    );
}

#[test]
fn an_on_read_template_prints_exactly_its_output() {
    let (_d, mut s) = store();
    let mut r = Rules::new("slack");
    r.template = Some(".payload.text".into());
    s.subscribe("inbox", "a", Some(r), 0).unwrap();
    put(&mut s, "slack", json!({"text": "hi"}), 1);
    let out = pull(&mut s, "inbox", "a", 2);
    assert_eq!(out.lines().nth(1), Some("hi"));
}

// 3. Names and the minimum budget.

#[test]
fn names_outside_the_allowed_set_are_refused_at_subscribe() {
    let (_d, mut s) = store();
    let long = "n".repeat(65);
    for bad in ["has space", "new\nline", "tab\there", "é", "a/b", "", &long] {
        let e = s.subscribe(bad, "a", Some(Rules::new("q")), 0);
        assert!(
            matches!(e, Err(tundish::Error::Usage(_))),
            "subscription {bad:?}"
        );
        let e = s.subscribe("feed", bad, Some(Rules::new("q")), 0);
        assert!(
            matches!(e, Err(tundish::Error::Usage(_))),
            "subscriber {bad:?}"
        );
    }
    let max = "n".repeat(64);
    for good in ["a", "Agent-1", "repo.co:review_bot", &max] {
        s.subscribe(good, good, Some(Rules::new("q")), 0)
            .unwrap_or_else(|e| panic!("{good:?}: {e}"));
    }
}

#[test]
fn the_cli_refuses_a_bad_name_with_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["has space", "new\nline"] {
        let o = cli(dir.path(), &["subscribe", bad, "--as", "a", "--queue", "q"]);
        assert_eq!(o.status.code(), Some(2), "{bad:?}");
        let o = cli(
            dir.path(),
            &["subscribe", "feed", "--as", bad, "--queue", "q"],
        );
        assert_eq!(o.status.code(), Some(2), "{bad:?}");
    }
}

#[test]
fn the_minimum_budget_holds_the_longest_status_line_and_a_cut_item() {
    let (_d, mut s) = store();
    let name = "s".repeat(64);
    let who = "w".repeat(64);
    s.subscribe(&name, &who, Some(on_ack("q")), 0).unwrap();
    let id = put(&mut s, "q", json!({"text": "x".repeat(5000)}), 1);
    let out = pull_at(&mut s, &name, &who, &budget(2, MIN_BYTES));
    assert!(out.len() <= MIN_BYTES, "{} > {MIN_BYTES}: {out}", out.len());
    assert!(
        out.ends_with(&format!(" … tundish show {id}\n")),
        "the pointer is whole: {out}"
    );
    assert_eq!(ids(&out), vec![id]);
    assert_eq!(pull(&mut s, &name, &who, 3), "", "the cursor moved");
}

#[test]
fn a_budget_below_the_minimum_is_refused() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    for small in [128, MIN_BYTES - 1] {
        assert!(
            s.pull("feed", "a", &budget(1, small), &mut Vec::new())
                .is_err()
        );
        assert!(s.pull_due("a", &budget(1, small), &mut Vec::new()).is_err());
    }
}

// 4. Progress.

#[test]
fn every_named_pull_delivers_its_first_candidate() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let p: Vec<i64> = (0..3)
        .map(|n| put(&mut s, "q", json!({"n": n, "pad": "x".repeat(2000)}), 1))
        .collect();
    let mut got = Vec::new();
    for now in 2..5 {
        let out = pull_at(&mut s, "feed", "a", &budget(now, MIN_BYTES));
        assert!(out.len() <= MIN_BYTES, "{out}");
        got.extend(ids(&out));
    }
    assert_eq!(got, p, "one item per pull, in order");
}

#[test]
fn a_due_pull_keeps_its_budget_and_reaches_every_subscription() {
    let (_d, mut s) = store();
    for sub in ["a1", "b1", "c1"] {
        s.subscribe(sub, "z", Some(Rules::new(sub)), 0).unwrap();
    }
    let mut want = Vec::new();
    for sub in ["a1", "b1", "c1"] {
        for n in 0..2 {
            want.push(put(&mut s, sub, json!({"n": n, "pad": "x".repeat(300)}), 1));
        }
    }
    let mut got = Vec::new();
    for now in 2..12 {
        let out = pull_due(&mut s, "z", &budget(now, MIN_BYTES));
        assert!(out.len() <= MIN_BYTES, "{} bytes: {out}", out.len());
        got.extend(ids(&out));
    }
    got.sort_unstable();
    assert_eq!(got, want, "every item once");
}

#[test]
fn a_latest_subscription_with_no_room_keeps_its_items_for_the_next_due_pull() {
    let (_d, mut s) = store();
    s.subscribe("big", "z", Some(Rules::new("big")), 0).unwrap();
    let mut r = Rules::new("q");
    r.select = tundish::rules::Select::Latest;
    r.limit = Some(2);
    s.subscribe("recent", "z", Some(r), 0).unwrap();
    put(&mut s, "big", json!({"pad": "x".repeat(570)}), 1);
    let newest: Vec<i64> = (0..3)
        .map(|n| put(&mut s, "q", json!({"n": n, "pad": "x".repeat(200)}), 1))
        .collect();
    let first = pull_due(&mut s, "z", &PullOpts::at(2));
    assert!(ids(&first).iter().all(|id| !newest.contains(id)), "{first}");
    assert!(
        first.contains("subscription=recent subscriber=z delivered=0 more=1 "),
        "the agent learns to pull again: {first}"
    );
    let next = pull_due(&mut s, "z", &PullOpts::at(3));
    assert_eq!(ids(&next), newest[1..].to_vec(), "{next}");
}

// 5. One-time counters.

/// A filter that fails, with a long error text, on the bad item.
fn adds_one() -> Rules {
    let mut r = Rules::new("q");
    r.filter = Some(".payload.x + 1 > 0".into());
    r
}

fn bad_item(s: &mut Store, now: i64) -> i64 {
    put(s, "q", json!({"x": "y".repeat(1100)}), now)
}

#[test]
fn a_long_filter_error_never_stalls_a_named_pull() {
    for max in [1024, MIN_BYTES] {
        let (_d, mut s) = store();
        s.subscribe("feed", "a", Some(adds_one()), 0).unwrap();
        bad_item(&mut s, 1);
        let first = pull_at(&mut s, "feed", "a", &budget(2, max));
        assert!(first.len() <= max, "{max}: {} bytes", first.len());
        assert_eq!(count(&first, "feed", "errors"), 1, "{first}");
        assert!(!first.contains("yyyy"), "no error text: {first}");
        assert_eq!(pull_at(&mut s, "feed", "a", &budget(3, max)), "");
        let good = put(&mut s, "q", json!({"x": 1}), 4);
        let next = pull_at(&mut s, "feed", "a", &budget(5, max));
        assert!(next.len() <= max, "{max}: {next}");
        assert_eq!(ids(&next), vec![good], "{max}: {next}");
        assert_eq!(count(&next, "feed", "errors"), 0, "counted once: {next}");
    }
}

#[test]
fn a_long_filter_error_never_stalls_a_due_pull() {
    for max in [1024, MIN_BYTES] {
        let (_d, mut s) = store();
        s.subscribe("feed", "a", Some(adds_one()), 0).unwrap();
        bad_item(&mut s, 1);
        let good = put(&mut s, "q", json!({"x": 1}), 1);
        let first = pull_due(&mut s, "a", &budget(2, max));
        assert!(first.len() <= max, "{max}: {} bytes", first.len());
        assert_eq!(ids(&first), vec![good], "{max}: {first}");
        assert_eq!(count(&first, "feed", "errors"), 1, "{first}");
        let later = put(&mut s, "q", json!({"x": 2}), 3);
        let next = pull_due(&mut s, "a", &budget(4, max));
        assert_eq!(ids(&next), vec![later], "{max}: {next}");
        assert_eq!(count(&next, "feed", "errors"), 0, "counted once: {next}");
    }
}

#[test]
fn a_filter_error_behind_a_limit_is_counted_once() {
    let (_d, mut s) = store();
    let mut r = adds_one();
    r.limit = Some(1);
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    let g1 = put(&mut s, "q", json!({"x": 1}), 1);
    bad_item(&mut s, 1);
    let g2 = put(&mut s, "q", json!({"x": 2}), 1);
    let first = pull(&mut s, "feed", "a", 2);
    assert_eq!(ids(&first), vec![g1]);
    assert_eq!(
        count(&first, "feed", "errors"),
        0,
        "the cursor stopped before the bad item: {first}"
    );
    let second = pull(&mut s, "feed", "a", 3);
    assert_eq!(ids(&second), vec![g2]);
    assert_eq!(count(&second, "feed", "errors"), 1, "{second}");
    assert_eq!(pull(&mut s, "feed", "a", 4), "");
}

#[test]
fn a_share_one_filter_error_is_counted_once_across_workers() {
    let (_d, mut s) = store();
    let mut r = adds_one();
    r.share = Share::One;
    r.consume = Consume::OnAck;
    r.limit = Some(1);
    s.subscribe("work", "a", Some(r), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let g1 = put(&mut s, "q", json!({"x": 1}), 1);
    bad_item(&mut s, 1);
    let g2 = put(&mut s, "q", json!({"x": 2}), 1);
    let first = pull(&mut s, "work", "a", 2);
    assert_eq!(ids(&first), vec![g1]);
    let second = pull(&mut s, "work", "b", 3);
    assert_eq!(ids(&second), vec![g2]);
    assert_eq!(
        count(&second, "work", "errors"),
        1,
        "the scan passed the bad item, whatever a holds: {second}"
    );
    s.ack(g1, "a", 4).unwrap();
    assert_eq!(pull(&mut s, "work", "a", 5), "", "counted once");
    assert_eq!(pull(&mut s, "work", "b", 6), "");
}

#[test]
fn status_keeps_the_last_error_text() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(adds_one()), 0).unwrap();
    bad_item(&mut s, 1);
    pull(&mut s, "feed", "a", 2);
    let st = s.status(3).unwrap();
    assert_eq!(st[0].errors, 0, "reported, so zeroed: {st:?}");
    let last = st[0].last_error.as_deref().expect("kept after the report");
    assert!(last.contains("yyy"), "{last}");
}

#[test]
fn a_later_clean_pull_keeps_the_last_error_text() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(adds_one()), 0).unwrap();
    bad_item(&mut s, 1);
    pull(&mut s, "feed", "a", 2);
    put(&mut s, "q", json!({"x": 1}), 3);
    pull(&mut s, "feed", "a", 4);
    let st = s.status(5).unwrap();
    let last = st[0].last_error.as_deref().expect("kept past a clean pull");
    assert!(last.contains("yyy"), "{last}");

    let mut one = adds_one();
    one.share = Share::One;
    s.subscribe("work", "w", Some(one), 6).unwrap();
    bad_item(&mut s, 7);
    pull(&mut s, "work", "w", 8);
    put(&mut s, "q", json!({"x": 2}), 9);
    pull(&mut s, "work", "w", 10);
    let st = s.status(11).unwrap();
    let work = st.iter().find(|r| r.subscription == "work").unwrap();
    assert!(
        work.last_error
            .as_deref()
            .is_some_and(|e| e.contains("yyy")),
        "{work:?}"
    );
}

#[test]
fn a_status_only_pull_does_not_start_the_every_window() {
    let (_d, mut s) = store();
    let mut r = adds_one();
    r.every = Some(300);
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    bad_item(&mut s, 1);
    let first = pull_due(&mut s, "a", &PullOpts::at(2));
    assert_eq!(count(&first, "feed", "errors"), 1, "{first}");
    assert!(ids(&first).is_empty(), "{first}");
    let good = put(&mut s, "q", json!({"x": 1}), 3);
    assert_eq!(ids(&pull_due(&mut s, "a", &PullOpts::at(4))), vec![good]);
}

#[test]
fn a_parked_item_behind_a_live_lease_is_reported_once() {
    let (_d, mut s) = store();
    // Item 2 parks while item 1 is still under a's live lease.
    let mut r = on_ack("q");
    r.share = Share::One;
    r.lease = 1000;
    r.max_attempts = 2;
    r.limit = Some(1);
    s.subscribe("jobs", "a", Some(r.clone()), 0).unwrap();
    s.subscribe("jobs", "b", None, 0).unwrap();
    let first = put(&mut s, "q", json!({"n": 1}), 1);
    let second = put(&mut s, "q", json!({"n": 2}), 1);
    assert_eq!(ids(&pull(&mut s, "jobs", "a", 2)), vec![first]);
    r.lease = 5;
    s.subscribe("jobs", "a", Some(r), 2).unwrap();
    assert_eq!(ids(&pull(&mut s, "jobs", "b", 3)), vec![second]);
    assert_eq!(ids(&pull(&mut s, "jobs", "b", 9)), vec![second]);
    let mut reports = 0;
    for now in [15, 16, 17] {
        let out = pull(&mut s, "jobs", "b", now);
        reports += i64::from(field(&out, "jobs", "parked").is_some_and(|v| v != "0"));
    }
    s.ack(first, "a", 18).unwrap();
    for now in [19, 20] {
        let out = pull(&mut s, "jobs", "b", now);
        reports += i64::from(field(&out, "jobs", "parked").is_some_and(|v| v != "0"));
    }
    assert_eq!(reports, 1, "parked must be reported exactly once");
}

#[test]
fn several_due_subscriptions_stay_inside_the_budget_and_all_deliver() {
    let (_d, mut s) = store();
    let who = "w".repeat(64);
    let mut want = Vec::new();
    for i in 0..4 {
        let name = format!("{i}{}", "s".repeat(32));
        let q = format!("q{i}");
        s.subscribe(&name, &who, Some(Rules::new(&q)), 0).unwrap();
        for n in 0..if i == 0 { 20 } else { 1 } {
            want.push(put(&mut s, &q, json!({"n": n, "pad": "x".repeat(40)}), 1));
        }
    }
    let mut got = Vec::new();
    for now in 2..30 {
        let out = pull_due(&mut s, &who, &budget(now, 1024));
        assert!(out.len() <= 1024, "{} bytes at {now}", out.len());
        got.extend(ids(&out));
    }
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
}

#[test]
fn a_parked_item_is_reported_once_and_stays_visible_in_status() {
    let (_d, mut s) = store();
    let mut r = on_ack("q");
    r.share = Share::One;
    r.lease = 10;
    r.max_attempts = 1;
    s.subscribe("jobs", "a", Some(r), 0).unwrap();
    let id = put(&mut s, "q", json!({}), 1);
    assert_eq!(ids(&pull(&mut s, "jobs", "a", 2)), vec![id]);
    let parked = pull(&mut s, "jobs", "a", 20);
    assert_eq!(count(&parked, "jobs", "parked"), 1, "{parked}");
    assert!(ids(&parked).is_empty(), "{parked}");
    assert_eq!(pull(&mut s, "jobs", "a", 30), "", "reported once");
    let st = s.status(40).unwrap();
    assert_eq!(st[0].parked, 1, "{st:?}");
}

#[test]
fn a_share_one_drop_is_reported_once_for_the_subscription() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.share = Share::One;
    s.subscribe("work", "a", Some(r), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    put(&mut s, "q", json!({}), 1);
    let aged = Retention {
        max_items: None,
        max_age: Some(10),
    };
    let fresh = s.publish("q", None, &json!({}), 100, aged).unwrap();
    assert_eq!(s.status(100).unwrap()[0].dropped, 1);
    let first = pull(&mut s, "work", "a", 101);
    assert_eq!(count(&first, "work", "dropped"), 1, "{first}");
    assert_eq!(ids(&first), vec![fresh]);
    assert_eq!(pull(&mut s, "work", "b", 102), "", "b is not told again");
}

#[test]
fn status_shows_errors_parked_dropped_and_the_last_error() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    assert!(
        cli(d, &["subscribe", "feed", "--as", "a", "--queue", "q"])
            .status
            .success()
    );
    let out = String::from_utf8(cli(d, &["status"]).stdout).unwrap();
    assert_eq!(
        out,
        "feed\ta\tqueue=q\tcursor=0\tunread=0\tnext_at=0\tdropped=0\terrors=0\tparked=0\tlast_error=\n"
    );
}

// 6. Hidden database flags.

#[test]
fn db_and_config_are_hidden_but_still_parsed() {
    let help = Command::new(env!("CARGO_BIN_EXE_tundish"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(!help.contains("--db"), "{help}");
    assert!(!help.contains("--config"), "{help}");
    let dir = tempfile::tempdir().unwrap();
    assert!(cli(dir.path(), &["check"]).status.success());
}

// 7. Prime.

#[test]
fn prime_names_the_full_ack_command() {
    assert!(
        tundish::cli::prime()
            .lines()
            .any(|l| l == "  tundish ack <id> --as <subscriber> [--subscription <s>]"),
        "{}",
        tundish::cli::prime()
    );
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
