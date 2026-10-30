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
    assert!(
        first.contains("tundish ack"),
        "the batch says how to ack: {first}"
    );
    s.ack(p[0], "a", 3).unwrap();
    let second = pull(&mut s, "work", "a", 4);
    assert_eq!(ids(&second), Vec::<i64>::new());
    assert!(second.contains("1 unacked"), "{second}");
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
    assert!(third.contains("1 parked"), "{third}");
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
    opts.max_bytes = 200;
    let out = pull_with(&mut s, "feed", "a", opts);
    assert!(out.len() <= 200, "{} bytes", out.len());
    assert!(out.contains(&format!("tundish show {id}")), "{out}");
    assert_eq!(pull(&mut s, "feed", "a", 3), "");
}

#[test]
fn the_byte_budget_stops_before_an_item_that_does_not_fit() {
    let (_d, mut s) = store();
    s.subscribe("feed", "a", Some(Rules::new("q")), 0).unwrap();
    let p = publish(&mut s, "q", 20, 1);
    let mut opts = PullOpts::at(2);
    opts.max_bytes = 120;
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
    assert!(out.contains("dropped"), "forced drops are reported: {out}");
}

#[test]
fn a_custom_template_renders_each_item() {
    let (_d, mut s) = store();
    let mut r = Rules::new("q");
    r.template = Some(r##""#\(.id) n=\(.payload.n)""##.into());
    s.subscribe("feed", "a", Some(r), 0).unwrap();
    let p = publish(&mut s, "q", 1, 1);
    assert_eq!(pull(&mut s, "feed", "a", 2), format!("#{} n=0\n", p[0]));
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
