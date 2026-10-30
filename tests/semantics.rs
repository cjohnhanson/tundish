//! The delivery matrix, one test per behaviour, against the library.

use serde_json::json;
use tundish::rules::{Consume, Rules, Select, Share, Start};
use tundish::{AckOutcome, PullOpts, Retention, Store};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(&dir.path().join("t.db")).unwrap();
    (dir, s)
}

fn pull(s: &mut Store, sub: &str, who: &str, now: i64) -> String {
    pull_with(s, sub, who, PullOpts::at(now))
}

fn pull_with(s: &mut Store, sub: &str, who: &str, opts: PullOpts) -> String {
    let mut out = Vec::new();
    s.pull(sub, who, &opts, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn ids(text: &str) -> Vec<i64> {
    text.lines()
        .filter_map(|l| l.strip_prefix('[')?.split_once(']'))
        .filter_map(|(tag, _)| tag.rsplit_once('#')?.1.parse().ok())
        .collect()
}

fn publish(s: &mut Store, q: &str, n: i64, now: i64) -> Vec<i64> {
    (0..n)
        .map(|i| {
            s.publish(
                q,
                None,
                &json!({"n": i, "even": i % 2 == 0}),
                now,
                Retention::default(),
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn all_on_read_shows_each_item_once_per_subscriber() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    s.subscribe("feed", "b", None, 0).unwrap();
    let p = publish(&mut s, "q", 3, 1);
    assert_eq!(ids(&pull(&mut s, "feed", "a", 2)), p);
    assert_eq!(ids(&pull(&mut s, "feed", "a", 3)), Vec::<i64>::new());
    assert_eq!(ids(&pull(&mut s, "feed", "b", 3)), p);
}

#[test]
fn start_all_keeps_items_published_before_the_first_pull() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let p = publish(&mut s, "q", 2, 1);
    s.subscribe("feed", "late", None, 5).unwrap();
    assert_eq!(ids(&pull(&mut s, "feed", "late", 6)), p);
}

#[test]
fn start_new_skips_the_backlog() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.start = Start::New;
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    publish(&mut s, "q", 2, 1);
    s.subscribe("feed", "late", None, 5).unwrap();
    let after = publish(&mut s, "q", 1, 6);
    assert_eq!(ids(&pull(&mut s, "feed", "late", 7)), after);
}

#[test]
fn a_late_worker_does_not_skip_a_share_one_backlog() {
    let (_d, mut s) = store();
    publish(&mut s, "q", 1, 0);
    let mut r = Rules::new("q");
    r.share = Share::One;
    r.start = Start::New;
    s.subscribe("work", "alice", Some(r), 1).unwrap();
    let mut owed = publish(&mut s, "q", 3, 2);
    s.subscribe("work", "bob", None, 3).unwrap();
    owed.extend(publish(&mut s, "q", 1, 4));
    let mut got = ids(&pull(&mut s, "work", "bob", 5));
    got.extend(ids(&pull(&mut s, "work", "alice", 6)));
    got.sort_unstable();
    assert_eq!(got, owed, "every item after the subscription began");
}

#[test]
fn filter_and_limit() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.filter = Some(".payload.even".into());
    r.limit = Some(2);
    s.subscribe("evens", "a", Some(r), 0).unwrap();
    let p = publish(&mut s, "q", 7, 1); // evens: p[0], p[2], p[4], p[6]
    assert_eq!(ids(&pull(&mut s, "evens", "a", 2)), vec![p[0], p[2]]);
    assert_eq!(ids(&pull(&mut s, "evens", "a", 3)), vec![p[4], p[6]]);
    assert_eq!(ids(&pull(&mut s, "evens", "a", 4)), Vec::<i64>::new());
}

#[test]
fn latest_takes_the_newest_unseen_and_moves_the_cursor_to_the_head() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.select = Select::Latest;
    r.limit = Some(2);
    s.subscribe("recent", "a", Some(r), 0).unwrap();
    let p = publish(&mut s, "q", 5, 1);
    assert_eq!(ids(&pull(&mut s, "recent", "a", 2)), vec![p[3], p[4]]);
    assert_eq!(ids(&pull(&mut s, "recent", "a", 3)), Vec::<i64>::new());
    let q = publish(&mut s, "q", 1, 4);
    assert_eq!(ids(&pull(&mut s, "recent", "a", 5)), q);
}

#[test]
fn all_on_ack_first_ack_wins_and_the_second_learns_who() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.consume = Consume::OnAck;
    s.subscribe("work", "a", Some(r), 0).unwrap();
    s.subscribe("work", "b", None, 0).unwrap();
    let p = publish(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "work", "a", 2)), p);
    assert_eq!(ids(&pull(&mut s, "work", "b", 2)), p);
    assert_eq!(s.ack(p[0], "a", 3).unwrap(), AckOutcome::Acked);
    assert_eq!(
        s.ack(p[0], "a", 4).unwrap(),
        AckOutcome::Acked,
        "a repeat by the owner is idempotent"
    );
    assert_eq!(
        s.ack(p[0], "b", 5).unwrap(),
        AckOutcome::AlreadyAcked {
            owner: "a".into(),
            at: 3
        }
    );
}

#[test]
fn all_on_ack_shows_an_item_once_and_counts_what_is_open() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.consume = Consume::OnAck;
    s.subscribe("work", "a", Some(r), 0).unwrap();
    let p = publish(&mut s, "q", 2, 1);
    let first = pull(&mut s, "work", "a", 2);
    assert_eq!(ids(&first), p);
    s.ack(p[0], "a", 3).unwrap();
    assert_eq!(pull(&mut s, "work", "a", 4), "", "nothing new to show");
    let q = publish(&mut s, "q", 1, 5);
    let third = pull(&mut s, "work", "a", 6);
    assert_eq!(ids(&third), q);
    assert!(third.contains(" unacked=1 "), "{third}");
}

#[test]
fn one_on_read_gives_each_item_to_one_puller() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.share = Share::One;
    r.limit = Some(1);
    s.subscribe("jobs", "a", Some(r), 0).unwrap();
    s.subscribe("jobs", "b", None, 0).unwrap();
    let p = publish(&mut s, "q", 2, 1);
    assert_eq!(ids(&pull(&mut s, "jobs", "a", 2)), vec![p[0]]);
    assert_eq!(ids(&pull(&mut s, "jobs", "b", 2)), vec![p[1]]);
    assert_eq!(ids(&pull(&mut s, "jobs", "a", 3)), Vec::<i64>::new());
}

#[test]
fn one_on_ack_leases_and_an_expired_lease_returns_the_item() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.share = Share::One;
    r.consume = Consume::OnAck;
    r.lease = 60;
    s.subscribe("jobs", "a", Some(r), 0).unwrap();
    s.subscribe("jobs", "b", None, 0).unwrap();
    let p = publish(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "jobs", "a", 10)), p);
    assert_eq!(ids(&pull(&mut s, "jobs", "b", 20)), Vec::<i64>::new());
    assert_eq!(
        s.ack(p[0], "b", 30).unwrap(),
        AckOutcome::LeasedBy {
            owner: "a".into(),
            until: 70
        }
    );
    assert_eq!(ids(&pull(&mut s, "jobs", "b", 71)), p, "the lease expired");
    assert_eq!(s.ack(p[0], "b", 72).unwrap(), AckOutcome::Acked);
    assert_eq!(ids(&pull(&mut s, "jobs", "a", 200)), Vec::<i64>::new());
}

#[test]
fn one_on_ack_parks_an_item_after_max_attempts() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.share = Share::One;
    r.consume = Consume::OnAck;
    r.lease = 10;
    r.max_attempts = 2;
    s.subscribe("jobs", "a", Some(r), 0).unwrap();
    let p = publish(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull(&mut s, "jobs", "a", 2)), p);
    assert_eq!(ids(&pull(&mut s, "jobs", "a", 20)), p);
    let third = pull(&mut s, "jobs", "a", 40);
    assert_eq!(ids(&third), Vec::<i64>::new());
    assert!(third.contains(" parked=1 "), "{third}");
}

#[test]
fn an_oversize_first_item_is_cut_and_the_cursor_moves() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let big = "x".repeat(5000);
    let id = s
        .publish("q", None, &json!({"text": big}), 1, Retention::default())
        .unwrap();
    let mut opts = PullOpts::at(2);
    opts.max_bytes = tundish::store::MIN_BYTES;
    let out = pull_with(&mut s, "feed", "a", opts);
    assert!(
        out.len() <= tundish::store::MIN_BYTES,
        "{} bytes",
        out.len()
    );
    assert!(out.contains(&format!("tundish show {id}")), "{out}");
    assert_eq!(pull(&mut s, "feed", "a", 3), "");
}

#[test]
fn the_byte_budget_stops_before_an_item_that_does_not_fit() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let p = publish(&mut s, "q", 20, 1);
    let mut opts = PullOpts::at(2);
    opts.max_bytes = 600;
    let first = ids(&pull_with(&mut s, "feed", "a", opts));
    assert!(!first.is_empty() && first.len() < 20);
    let rest = ids(&pull(&mut s, "feed", "a", 3));
    assert_eq!([first, rest].concat(), p, "nothing lost, nothing repeated");
}

#[test]
fn a_passed_deadline_rolls_back() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let p = publish(&mut s, "q", 2, 1);
    let mut opts = PullOpts::at(2);
    opts.deadline = Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
    let mut out = Vec::new();
    assert!(matches!(
        s.pull("feed", "a", &opts, &mut out),
        Err(tundish::Error::Deadline)
    ));
    assert_eq!(
        ids(&pull(&mut s, "feed", "a", 3)),
        p,
        "nothing was marked delivered"
    );
}

#[test]
fn due_honours_every_and_only_after_a_delivery() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.every = Some(300);
    s.subscribe("digest", "a", Some(r), 0).unwrap();
    s.subscribe("other", "b", Some(Rules::new("q")), 0).unwrap();
    assert_eq!(s.due("a", 10).unwrap(), vec!["digest".to_string()]);
    assert_eq!(pull(&mut s, "digest", "a", 10), "", "nothing published yet");
    assert_eq!(
        s.due("a", 11).unwrap(),
        vec!["digest".to_string()],
        "an empty pull spends no window"
    );
    publish(&mut s, "q", 1, 12);
    assert!(!pull(&mut s, "digest", "a", 13).is_empty());
    assert!(s.due("a", 200).unwrap().is_empty());
    assert_eq!(s.due("a", 313).unwrap(), vec!["digest".to_string()]);
}

#[test]
fn not_subscribed_is_an_error_that_names_the_fix() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let err = s
        .pull("feed", "zed", &PullOpts::at(1), &mut Vec::new())
        .unwrap_err();
    assert!(
        err.to_string().contains("tundish subscribe feed --as zed"),
        "{err}"
    );
}

#[test]
fn retention_never_trims_past_an_unread_cursor_except_by_age() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let keep = Retention {
        max_items: Some(2),
        max_age: None,
    };
    let p: Vec<i64> = (0..4)
        .map(|i| s.publish("q", None, &json!({"n": i}), 1, keep).unwrap())
        .collect();
    assert_eq!(
        ids(&pull(&mut s, "feed", "a", 2)),
        p,
        "max_items waits for the reader"
    );
    s.publish("q", None, &json!({"n": 9}), 3, keep).unwrap();
    assert_eq!(s.count("q").unwrap(), 2, "read items now trim");

    let aged = Retention {
        max_items: None,
        max_age: Some(10),
    };
    s.publish("q", None, &json!({"n": 10}), 100, aged).unwrap();
    let out = pull(&mut s, "feed", "a", 101);
    assert!(
        out.contains(" dropped=1 "),
        "forced drops are reported: {out}"
    );
}

#[test]
fn a_custom_template_renders_each_item() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.template = Some(r##""#\(.id) n=\(.payload.n)""##.into());
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    let p = publish(&mut s, "q", 1, 1);
    assert_eq!(
        pull(&mut s, "feed", "a", 2),
        format!(
            "tundish: subscription=feed subscriber=a delivered=1 more=0 unacked=0 parked=0 errors=0 dropped=0 lease=0\n#{} n=0\n",
            p[0]
        )
    );
}

fn pull_due(s: &mut Store, who: &str, opts: PullOpts) -> String {
    let mut out = Vec::new();
    s.pull_due(who, &opts, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn the_unacked_count_prints_only_with_delivered_items() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.consume = Consume::OnAck;
    r.every = Some(300);
    s.subscribe("inbox", "a", Some(r), 0).unwrap();
    let p = publish(&mut s, "q", 1, 1);
    assert_eq!(ids(&pull_due(&mut s, "a", PullOpts::at(2))), p);
    for now in [400, 401, 402] {
        assert_eq!(
            pull_due(&mut s, "a", PullOpts::at(now)),
            "",
            "an unacked count alone is not a delivery"
        );
    }
    assert_eq!(
        pull(&mut s, "inbox", "a", 403),
        "",
        "a named pull follows the same rule"
    );
    let q = publish(&mut s, "q", 1, 404);
    let out = pull_due(&mut s, "a", PullOpts::at(405));
    assert_eq!(ids(&out), q);
    assert!(
        out.contains(" unacked=1 "),
        "the count rides with items: {out}"
    );
}

#[test]
fn a_due_pull_reports_filter_errors_once_even_with_no_items() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.filter = Some(".payload.a.b == 1".into());
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    s.publish("q", None, &json!({"a": 5}), 1, Retention::default())
        .unwrap();
    let out = pull_due(&mut s, "a", PullOpts::at(2));
    assert!(
        out.contains(" delivered=0 ") && out.contains(" errors=1 "),
        "silently skipped: {out:?}"
    );
    assert_eq!(pull_due(&mut s, "a", PullOpts::at(3)), "", "reported once");
}

#[test]
fn an_unbound_share_one_subscription_does_not_hold_retention() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.share = Share::One;
    s.subscribe("work", "w", Some(r), 0).unwrap();
    s.unsubscribe("work", "w").unwrap();
    let keep = Retention {
        max_items: Some(2),
        max_age: None,
    };
    for i in 0..6 {
        s.publish("q", None, &json!({"n": i}), 1, keep).unwrap();
    }
    assert_eq!(s.count("q").unwrap(), 2);
}

#[test]
fn a_due_pull_does_not_starve_later_subscriptions() {
    let (_d, mut s) = store();
    s.subscribe("a1", "z", Some(Rules::new("q1")), 0).unwrap();
    s.subscribe("b1", "z", Some(Rules::new("q2")), 0).unwrap();
    publish(&mut s, "q1", 40, 1);
    let b = publish(&mut s, "q2", 1, 1);
    let mut seen = Vec::new();
    for now in [2, 3] {
        let mut opts = PullOpts::at(now);
        // Room for one subscription's status line and items per pull.
        opts.max_bytes = tundish::store::MIN_BYTES;
        seen.extend(ids(&pull_due(&mut s, "z", opts)));
    }
    assert!(seen.contains(&b[0]), "b1 waited behind a1: {seen:?}");
}

#[test]
fn latest_under_a_tight_budget_keeps_the_newest() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.select = Select::Latest;
    r.limit = Some(3);
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    let p: Vec<i64> = (0..5)
        .map(|i| {
            s.publish(
                "q",
                None,
                &json!({"n": i, "pad": "x".repeat(100)}),
                1,
                Retention::default(),
            )
            .unwrap()
        })
        .collect();
    // Each item line is 123 bytes; the status line reserve for feed and
    // a is 213. 513 bytes hold two items, not three.
    let mut opts = PullOpts::at(2);
    opts.max_bytes = 513;
    let out = pull_with(&mut s, "feed", "a", opts);
    assert_eq!(ids(&out), p[3..].to_vec(), "{out}");
    assert!(out.contains(" more=0 "), "{out}");
}

#[test]
fn changing_share_on_the_same_queue_keeps_positions() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    publish(&mut s, "q", 3, 1);
    assert_eq!(ids(&pull(&mut s, "feed", "a", 2)).len(), 3);
    let mut one = Rules::new("q");
    one.share = Share::One;
    s.subscribe("feed", "a", Some(one), 3).unwrap();
    let next = publish(&mut s, "q", 1, 4);
    assert_eq!(ids(&pull(&mut s, "feed", "a", 5)), next, "all to one");
    s.subscribe("feed", "a", Some(Rules::new("q")), 6).unwrap();
    let last = publish(&mut s, "q", 1, 7);
    assert_eq!(ids(&pull(&mut s, "feed", "a", 8)), last, "one to all");
}

#[test]
fn idle_due_subscriptions_do_not_starve_one_with_items() {
    for (idle, long) in [(4, false), (2, true)] {
        let (_d, mut s) = store();
        let who = if long { "w".repeat(64) } else { "w".into() };
        let name = |i: usize| {
            if long {
                format!("{i}{}", "s".repeat(63))
            } else {
                format!("a{i}")
            }
        };
        for i in 0..=idle {
            s.subscribe(&name(i), &who, Some(Rules::new(&format!("q{i}"))), 0)
                .unwrap();
        }
        let last = publish(&mut s, &format!("q{idle}"), 1, 1);
        let got = ids(&pull_due(&mut s, &who, PullOpts::at(2)));
        assert_eq!(got, last, "{idle} idle, long names {long}");
    }
}

#[test]
fn a_switch_to_on_read_does_not_pin_a_parked_item() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.share = Share::One;
    r.consume = Consume::OnAck;
    r.max_attempts = 1;
    r.lease = 10;
    s.subscribe("w", "a", Some(r.clone()), 100).unwrap();
    publish(&mut s, "q", 1, 100);
    pull(&mut s, "w", "a", 100);
    r.consume = Consume::OnRead;
    s.subscribe("w", "a", Some(r), 105).unwrap();
    let later = publish(&mut s, "q", 1, 106);
    assert_eq!(ids(&pull(&mut s, "w", "a", 200)), later);
    pull(&mut s, "w", "a", 201);
    let st = s.status(202).unwrap();
    assert_eq!(st[0].unread, 0, "the parked item pins the position: {st:?}");
}

#[test]
fn pull_due_commits_every_subscription_or_none() {
    let (_d, mut s) = store();
    s.subscribe("one", "a", Some(Rules::new("q1")), 0).unwrap();
    s.subscribe("two", "a", Some(Rules::new("q2")), 0).unwrap();
    let p1 = publish(&mut s, "q1", 1, 1);
    let p2 = publish(&mut s, "q2", 1, 1);
    let mut opts = PullOpts::at(2);
    opts.deadline = Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
    assert!(matches!(
        s.pull_due("a", &opts, &mut Vec::new()),
        Err(tundish::Error::Deadline)
    ));
    let mut out = Vec::new();
    s.pull_due("a", &PullOpts::at(3), &mut out).unwrap();
    let got = ids(&String::from_utf8(out).unwrap());
    assert_eq!(
        got,
        [p1, p2].concat(),
        "the failed run marked nothing delivered"
    );
}
