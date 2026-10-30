//! The SQLite store: one file, one schema, every write in one
//! `BEGIN IMMEDIATE` transaction. Acks and leases belong to one
//! subscription, so two subscriptions on a queue never share a claim.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::expr::{self, Expr};
use crate::rules::{Consume, Rules, Select, Share, Start};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  queue TEXT NOT NULL,
  key TEXT,
  payload TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX items_queue ON items(queue, id);
CREATE TABLE subscriptions (
  name TEXT PRIMARY KEY,
  queue TEXT NOT NULL,
  rules TEXT NOT NULL,
  lwm INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  dropped INTEGER NOT NULL DEFAULT 0,
  errors INTEGER NOT NULL DEFAULT 0,
  parked INTEGER NOT NULL DEFAULT 0,
  last_error TEXT
);
CREATE TABLE subscribers (
  subscription TEXT NOT NULL REFERENCES subscriptions(name) ON DELETE CASCADE,
  subscriber TEXT NOT NULL,
  origin INTEGER NOT NULL,
  cursor INTEGER NOT NULL,
  next_at INTEGER NOT NULL DEFAULT 0,
  dropped INTEGER NOT NULL DEFAULT 0,
  errors INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  PRIMARY KEY (subscription, subscriber)
);
CREATE TABLE acks (
  subscription TEXT NOT NULL REFERENCES subscriptions(name) ON DELETE CASCADE,
  item INTEGER NOT NULL,
  owner TEXT NOT NULL,
  acked_at INTEGER NOT NULL,
  PRIMARY KEY (subscription, item)
);
CREATE TABLE leases (
  subscription TEXT NOT NULL REFERENCES subscriptions(name) ON DELETE CASCADE,
  item INTEGER NOT NULL,
  owner TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  attempts INTEGER NOT NULL,
  PRIMARY KEY (subscription, item)
);
";

/// Rows scanned per pull, so a pull holds the write lock briefly.
const SCAN: i64 = 1000;

/// The longest subscription or subscriber name.
pub const NAME_MAX: usize = 64;

/// Digits in the largest count a status line prints: `i64::MAX`.
const COUNT_DIGITS: usize = 19;

/// The status line's fixed text: its prefix, nine `key=`s, the spaces
/// between them, and the newline. 93 bytes.
const STATUS_TEXT: usize = "tundish: subscription= subscriber= delivered= more= unacked= parked= errors= dropped= lease=\n".len();

/// The longest status line for two names: the fixed text, a one-digit
/// `more`, the names, and six counts of at most `COUNT_DIGITS` digits.
const fn status_max(name: usize, subscriber: usize) -> usize {
    STATUS_TEXT + 1 + name + subscriber + 6 * COUNT_DIGITS
}

/// The longest status line: 93 + 1 + 64 + 64 + 6 × 19 = 336 bytes.
pub const STATUS_MAX: usize = status_max(NAME_MAX, NAME_MAX);

/// The cut pointer's fixed text, ` … tundish show ` and the newline.
const POINTER_TEXT: usize = " … tundish show \n".len();

/// The smallest slice of a first item: the longest cut pointer
/// (19 bytes of text, a 19-digit id: 38 bytes) and 26 bytes of the item.
const ITEM_SLICE: usize = 64;

/// The smallest byte budget a pull accepts: the longest status line and
/// the smallest item slice, so the first candidate always fits with an
/// uncut pointer. 336 + 64 = 400 bytes.
pub const MIN_BYTES: usize = STATUS_MAX + ITEM_SLICE;

const _: () = assert!(POINTER_TEXT + COUNT_DIGITS < ITEM_SLICE);

/// How long a queue keeps items.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    pub max_items: Option<i64>,
    /// Seconds.
    pub max_age: Option<i64>,
}

/// How a pull runs.
#[derive(Debug, Clone)]
pub struct PullOpts {
    /// The clock, in Unix seconds.
    pub now: i64,
    /// The budget for the whole printed batch, status lines included.
    pub max_bytes: usize,
    /// Past this instant, the pull rolls back instead of committing.
    pub deadline: Option<Instant>,
}

impl PullOpts {
    /// Defaults at a given clock.
    #[must_use]
    pub fn at(now: i64) -> Self {
        Self {
            now,
            max_bytes: 1024,
            deadline: None,
        }
    }
}

/// What an ack did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AckOutcome {
    Acked,
    AlreadyAcked { owner: String, at: i64 },
    LeasedBy { owner: String, until: i64 },
    NotFound,
}

/// What a subscribe did to the stored rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// No rules were given; the stored ones stand.
    None,
    Created,
    /// New rules on the same queue; positions and acks stand.
    Replaced,
    /// New rules on another queue; positions, acks, and leases reset.
    Moved {
        from: String,
    },
}

/// The result of a subscribe.
#[derive(Debug, Clone)]
pub struct Subscribed {
    pub change: Change,
    /// Whether this call bound the subscriber.
    pub bound: bool,
    pub rules: Rules,
}

/// One subscription's ack of an item.
#[derive(Debug, Clone, Serialize)]
pub struct Ack {
    pub subscription: String,
    pub owner: String,
    pub at: i64,
}

/// One stored item.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: i64,
    pub queue: String,
    pub key: Option<String>,
    pub payload: Value,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acks: Vec<Ack>,
}

/// One subscriber's state, for `status`.
#[derive(Debug, Clone, Serialize)]
pub struct SubscriberState {
    pub subscription: String,
    pub subscriber: String,
    pub queue: String,
    pub cursor: i64,
    pub unread: i64,
    pub next_at: i64,
    pub dropped: i64,
    pub errors: i64,
    pub parked: i64,
    pub last_error: Option<String>,
}

/// The store.
pub struct Store {
    conn: Connection,
}

struct Row {
    id: i64,
    queue: String,
    key: Option<String>,
    payload: String,
    created_at: i64,
}

impl Row {
    fn value(&self) -> Result<jaq_json::Val> {
        expr::value(&format!(
            r#"{{"id":{},"queue":{},"key":{},"payload":{},"created_at":{}}}"#,
            self.id,
            serde_json::to_string(&self.queue)?,
            serde_json::to_string(&self.key)?,
            self.payload,
            self.created_at
        ))
    }

    /// The default rendering, which never fails.
    fn plain(&self) -> String {
        let key = self
            .key
            .as_deref()
            .map(|k| format!(" {k}"))
            .unwrap_or_default();
        format!("[{}#{}]{key} {}", self.queue, self.id, self.payload)
    }
}

impl Store {
    /// Open or create the store at `path`, waiting up to five seconds
    /// for another writer.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, Duration::from_secs(5))
    }

    /// Open with an explicit wait for the write lock. Opening an
    /// existing store takes no write lock.
    pub fn open_with(path: &Path, busy: Duration) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(busy)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    fn migrate(&mut self) -> Result<()> {
        let version = |c: &Connection| -> Result<i64> {
            Ok(c.pragma_query_value(None, "user_version", |r| r.get(0))?)
        };
        let v = version(&self.conn)?;
        if v > SCHEMA_VERSION {
            return Err(Error::Usage(format!(
                "this database has schema {v}; this tundish knows {SCHEMA_VERSION}. Upgrade tundish."
            )));
        }
        if v == SCHEMA_VERSION {
            return Ok(());
        }
        self.conn.pragma_update(None, "journal_mode", "WAL")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if version(&tx)? == 0 {
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn write(&mut self) -> Result<rusqlite::Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// Publish one item. The insert and the retention trim share one
    /// transaction. Returns the item id.
    pub fn publish(
        &mut self,
        queue: &str,
        key: Option<&str>,
        payload: &Value,
        now: i64,
        retention: Retention,
    ) -> Result<i64> {
        if queue.is_empty() {
            return Err(Error::Usage("a queue name cannot be empty".into()));
        }
        let tx = self.write()?;
        tx.execute(
            "INSERT INTO items(queue, key, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![queue, key, serde_json::to_string(payload)?, now],
        )?;
        let id = tx.last_insert_rowid();
        trim(&tx, queue, now, retention)?;
        tx.commit()?;
        Ok(id)
    }

    /// Create a subscription (when `rules` is given) and bind
    /// `subscriber` to it. Given rules replace the stored ones; a move
    /// to another queue resets every position in the subscription.
    pub fn subscribe(
        &mut self,
        name: &str,
        subscriber: &str,
        rules: Option<Rules>,
        now: i64,
    ) -> Result<Subscribed> {
        check_name("subscription", name)?;
        check_name("subscriber", subscriber)?;
        let tx = self.write()?;
        let mut change = Change::None;
        if let Some(r) = &rules {
            r.check()?;
            // A share one subscription has one position for all its
            // subscribers, fixed when it is created or moved.
            let start = start_position(&tx, r)?;
            let old_share = load_rules(&tx, name).ok().map(|o| o.share);
            let old: Option<String> = tx
                .query_row(
                    "SELECT queue FROM subscriptions WHERE name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .optional()?;
            tx.execute(
                "INSERT INTO subscriptions(name, queue, rules, lwm, created_at)
                 VALUES (?1, ?2, ?3, ?5, ?4)
                 ON CONFLICT(name) DO UPDATE SET queue = excluded.queue, rules = excluded.rules",
                params![name, r.queue, serde_json::to_string(r)?, now, start],
            )?;
            change = match old {
                None => Change::Created,
                Some(q) if q == r.queue => Change::Replaced,
                Some(q) => Change::Moved { from: q },
            };
            if change == Change::Replaced && old_share != Some(r.share) {
                // Each share mode keeps its own position; carry the
                // read position across so no item comes back.
                match r.share {
                    Share::One => tx.execute(
                        "UPDATE subscriptions SET lwm = coalesce(
                           (SELECT min(cursor) FROM subscribers WHERE subscription = ?1), lwm)
                         WHERE name = ?1",
                        [name],
                    )?,
                    Share::All => tx.execute(
                        "UPDATE subscribers SET cursor = max(cursor,
                           (SELECT lwm FROM subscriptions WHERE name = ?1))
                         WHERE subscription = ?1",
                        [name],
                    )?,
                };
            }
            if matches!(change, Change::Moved { .. }) {
                tx.execute(
                    "UPDATE subscriptions SET lwm = ?2, dropped = 0, errors = 0, parked = 0,
                       last_error = NULL
                     WHERE name = ?1",
                    params![name, start],
                )?;
                tx.execute("DELETE FROM acks WHERE subscription = ?1", [name])?;
                tx.execute("DELETE FROM leases WHERE subscription = ?1", [name])?;
                tx.execute(
                    "UPDATE subscribers SET origin = ?2, cursor = ?2, dropped = 0, errors = 0,
                       last_error = NULL
                     WHERE subscription = ?1",
                    params![name, start],
                )?;
            }
        }
        let rules = load_rules(&tx, name)?;
        let start = start_position(&tx, &rules)?;
        let bound = tx.execute(
            "INSERT OR IGNORE INTO subscribers(subscription, subscriber, origin, cursor)
             VALUES (?1, ?2, ?3, ?3)",
            params![name, subscriber, start],
        )? > 0;
        tx.commit()?;
        Ok(Subscribed {
            change,
            bound,
            rules,
        })
    }

    /// Unbind a subscriber. The subscription and its acks remain.
    pub fn unsubscribe(&mut self, name: &str, subscriber: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM subscribers WHERE subscription = ?1 AND subscriber = ?2",
            params![name, subscriber],
        )?;
        Ok(n > 0)
    }

    /// The subscriptions `subscriber` is bound to whose `every` window
    /// has passed, least recently delivered first, so a backlog in one
    /// cannot hold the others behind it.
    pub fn due(&self, subscriber: &str, now: i64) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT subscription FROM subscribers WHERE subscriber = ?1 AND next_at <= ?2
             ORDER BY next_at, subscription",
        )?;
        let rows = st.query_map(params![subscriber, now], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Pull a batch for `subscriber`, print it to `out`, and commit
    /// what was printed. At-least-once: output is flushed before the
    /// commit, and a pull past its deadline rolls back.
    pub fn pull(
        &mut self,
        name: &str,
        subscriber: &str,
        opts: &PullOpts,
        out: &mut dyn Write,
    ) -> Result<usize> {
        check_budget(opts.max_bytes)?;
        let tx = self.write()?;
        let (text, n) = pull_in(&tx, name, subscriber, opts, opts.max_bytes, true)?;
        finish(tx, &text, opts, out)?;
        Ok(n)
    }

    /// Pull every due subscription of `subscriber` in one transaction,
    /// sharing one byte budget: all of it is marked delivered, or none.
    /// The budget holds one status line per subscription pulled; the
    /// subscriptions that do not fit wait for the next pull, first in
    /// line since they did not deliver.
    pub fn pull_due(
        &mut self,
        subscriber: &str,
        opts: &PullOpts,
        out: &mut dyn Write,
    ) -> Result<usize> {
        check_budget(opts.max_bytes)?;
        let bindings: i64 = self.conn.query_row(
            "SELECT count(*) FROM subscribers WHERE subscriber = ?1",
            [subscriber],
            |r| r.get(0),
        )?;
        if bindings == 0 {
            return Err(Error::Usage(format!(
                "{subscriber} holds no subscription, so nothing can be due: tundish subscribe <name> --as {subscriber}"
            )));
        }
        let due = self.due(subscriber, opts.now)?;
        let tx = self.write()?;
        let mut text = String::new();
        let mut n = 0;
        for name in &due {
            // Each subscription gets what is left. One that prints
            // nothing costs nothing; one with no room stays due and,
            // not having delivered, comes first next time.
            let left = opts.max_bytes.saturating_sub(text.len());
            if left < status_max(name.len(), subscriber.len()) + ITEM_SLICE {
                continue;
            }
            let (t, k) = pull_in(&tx, name, subscriber, opts, left, text.is_empty())?;
            text.push_str(&t);
            n += k;
        }
        finish(tx, &text, opts, out)?;
        Ok(n)
    }

    /// Ack an item for `subscriber` in the one subscription it holds
    /// on the item's queue.
    pub fn ack(&mut self, item: i64, subscriber: &str, now: i64) -> Result<AckOutcome> {
        self.ack_in(item, subscriber, None, now)
    }

    /// Ack an item in a named subscription, or in the one subscription
    /// `subscriber` holds on the item's queue. The first ack wins; a
    /// repeat by the same owner reports `Acked`.
    pub fn ack_in(
        &mut self,
        item: i64,
        subscriber: &str,
        subscription: Option<&str>,
        now: i64,
    ) -> Result<AckOutcome> {
        if subscriber.is_empty() {
            return Err(Error::Usage("an ack needs a subscriber".into()));
        }
        let tx = self.write()?;
        let queue: Option<String> = tx
            .query_row("SELECT queue FROM items WHERE id = ?1", [item], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(queue) = queue else {
            return Ok(AckOutcome::NotFound);
        };
        let mut st = tx.prepare(
            "SELECT p.name FROM subscriptions p JOIN subscribers s ON s.subscription = p.name
             WHERE s.subscriber = ?1 AND p.queue = ?2 ORDER BY p.name",
        )?;
        let held: Vec<String> = st
            .query_map(params![subscriber, queue], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        drop(st);
        let sub = match subscription {
            Some(s) if held.iter().any(|h| h == s) => s.to_owned(),
            Some(s) => {
                return Err(Error::Usage(format!(
                    "{subscriber} holds no subscription {s} on queue {queue}"
                )));
            }
            None => match held.as_slice() {
                [one] => one.clone(),
                [] => {
                    return Err(Error::Usage(format!(
                        "{subscriber} holds no subscription on queue {queue}"
                    )));
                }
                many => {
                    return Err(Error::Usage(format!(
                        "{subscriber} holds {} on queue {queue}; pass --subscription",
                        many.join(", ")
                    )));
                }
            },
        };
        if let Some((holder, until, _)) = lease_of(&tx, &sub, item)?
            && holder != subscriber
            && until > now
        {
            return Ok(AckOutcome::LeasedBy {
                owner: holder,
                until,
            });
        }
        let won: Option<i64> = tx
            .query_row(
                "INSERT INTO acks(subscription, item, owner, acked_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(subscription, item) DO NOTHING RETURNING item",
                params![sub, item, subscriber, now],
                |r| r.get(0),
            )
            .optional()?;
        let outcome = if won.is_some() {
            AckOutcome::Acked
        } else {
            let (holder, at): (String, i64) = tx.query_row(
                "SELECT owner, acked_at FROM acks WHERE subscription = ?1 AND item = ?2",
                params![sub, item],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if holder == subscriber {
                AckOutcome::Acked
            } else {
                AckOutcome::AlreadyAcked { owner: holder, at }
            }
        };
        tx.execute(
            "DELETE FROM leases WHERE subscription = ?1 AND item = ?2",
            params![sub, item],
        )?;
        tx.commit()?;
        Ok(outcome)
    }

    /// One item by id, with every subscription's ack.
    pub fn show(&self, id: i64) -> Result<Option<Item>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, queue, key, payload, created_at FROM items WHERE id = ?1",
                [id],
                row_from,
            )
            .optional()?;
        row.map(|r| item(&self.conn, r)).transpose()
    }

    /// Items in a queue, oldest first. With `unacked`, only items no
    /// subscription has acked, or, with `subscription`, items that
    /// subscription has not acked.
    pub fn list(
        &self,
        queue: &str,
        unacked: bool,
        subscription: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Item>> {
        let mut st = self.conn.prepare(
            "SELECT id, queue, key, payload, created_at FROM items i
             WHERE queue = ?1 AND (?2 = 0 OR NOT EXISTS (
               SELECT 1 FROM acks a WHERE a.item = i.id AND (?3 IS NULL OR a.subscription = ?3)))
             ORDER BY id DESC LIMIT ?4",
        )?;
        let rows: Vec<Row> = st
            .query_map(params![queue, unacked, subscription, limit], row_from)?
            .collect::<std::result::Result<_, _>>()?;
        let mut items = rows
            .into_iter()
            .map(|r| item(&self.conn, r))
            .collect::<Result<Vec<_>>>()?;
        items.reverse();
        Ok(items)
    }

    /// Queues with their item counts.
    pub fn queues(&self) -> Result<Vec<(String, i64)>> {
        let mut st = self
            .conn
            .prepare("SELECT queue, count(*) FROM items GROUP BY queue ORDER BY queue")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Items in one queue.
    pub fn count(&self, queue: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM items WHERE queue = ?1",
            [queue],
            |r| r.get(0),
        )?)
    }

    /// Every subscription and its rules.
    pub fn subscriptions(&self) -> Result<Vec<(String, Rules)>> {
        let mut st = self
            .conn
            .prepare("SELECT name, rules FROM subscriptions ORDER BY name")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (n, r) = row?;
            out.push((n, serde_json::from_str(&r)?));
        }
        Ok(out)
    }

    /// Every subscriber's position at clock `now`. For a `share: one`
    /// subscription the position, the unread count, and the unreported
    /// counts are the subscription's. `parked` counts the items parked
    /// now; `dropped` and `errors` count what no pull has reported yet.
    pub fn status(&self, now: i64) -> Result<Vec<SubscriberState>> {
        let mut st = self.conn.prepare(
            "SELECT s.subscription, s.subscriber, p.queue,
               CASE WHEN json_extract(p.rules, '$.share') = 'one' THEN p.lwm ELSE s.cursor END AS pos,
               s.next_at,
               CASE WHEN json_extract(p.rules, '$.share') = 'one' THEN p.dropped ELSE s.dropped END,
               CASE WHEN json_extract(p.rules, '$.share') = 'one'
                 THEN (SELECT count(*) FROM items i WHERE i.queue = p.queue AND i.id > p.lwm
                         AND NOT EXISTS (SELECT 1 FROM acks a WHERE a.subscription = p.name AND a.item = i.id))
                 ELSE (SELECT count(*) FROM items i WHERE i.queue = p.queue AND i.id > s.cursor) END,
               CASE WHEN json_extract(p.rules, '$.share') = 'one' THEN p.errors ELSE s.errors END,
               (SELECT count(*) FROM leases l WHERE l.subscription = p.name
                  AND l.attempts >= json_extract(p.rules, '$.max_attempts') AND l.expires_at <= ?1),
               CASE WHEN json_extract(p.rules, '$.share') = 'one' THEN p.last_error ELSE s.last_error END
             FROM subscribers s JOIN subscriptions p ON p.name = s.subscription
             ORDER BY s.subscription, s.subscriber",
        )?;
        let rows = st.query_map([now], |r| {
            Ok(SubscriberState {
                subscription: r.get(0)?,
                subscriber: r.get(1)?,
                queue: r.get(2)?,
                cursor: r.get(3)?,
                next_at: r.get(4)?,
                dropped: r.get(5)?,
                unread: r.get(6)?,
                errors: r.get(7)?,
                parked: r.get(8)?,
                last_error: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

fn check_budget(max_bytes: usize) -> Result<()> {
    if max_bytes < MIN_BYTES {
        return Err(Error::Usage(format!(
            "--max-bytes must be at least {MIN_BYTES}"
        )));
    }
    Ok(())
}

/// Print, flush, then commit unless the deadline passed.
fn finish(
    tx: rusqlite::Transaction<'_>,
    text: &str,
    opts: &PullOpts,
    out: &mut dyn Write,
) -> Result<()> {
    out.write_all(text.as_bytes())?;
    out.flush()?;
    if opts.deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(Error::Deadline); // the transaction drops and rolls back
    }
    tx.commit()?;
    Ok(())
}

/// Refuse a name the status line cannot carry: 1 to `NAME_MAX` bytes
/// of `A-Z a-z 0-9 . _ : -`, so the line has no spaces, newlines, or
/// `=` inside a field and a fixed maximum length.
fn check_name(kind: &str, name: &str) -> Result<()> {
    let ok = (1..=NAME_MAX).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "a {kind} name is 1 to {NAME_MAX} of A-Z a-z 0-9 . _ : -, not {name:?}"
        )))
    }
}

/// One subscription's status in one pull. Each field is a count or a
/// flag, so the line never carries payload or error text.
#[derive(Debug, Default)]
struct Status {
    delivered: usize,
    more: bool,
    /// Items shown in earlier pulls and not acked; set only alongside
    /// delivered items.
    unacked: i64,
    /// One-time counts, reported by the pull that zeroes them.
    parked: i64,
    errors: i64,
    dropped: i64,
    /// Seconds the delivered items stay leased, or 0.
    lease: i64,
}

impl Status {
    /// Whether the subscription prints anything in this pull.
    fn reports(&self) -> bool {
        self.delivered > 0 || self.more || self.parked > 0 || self.errors > 0 || self.dropped > 0
    }

    fn line(&self, name: &str, subscriber: &str) -> String {
        format!(
            "tundish: subscription={name} subscriber={subscriber} delivered={} more={} unacked={} parked={} errors={} dropped={} lease={}\n",
            self.delivered,
            u8::from(self.more),
            self.unacked,
            self.parked,
            self.errors,
            self.dropped,
            self.lease
        )
    }
}

/// One subscription's batch, recorded in `tx`, returned as its status
/// line and item lines in at most `budget` bytes, or as nothing when it
/// has nothing to report. Only the first item of a whole output
/// (`may_cut`) is cut to fit.
fn pull_in(
    tx: &Connection,
    name: &str,
    subscriber: &str,
    opts: &PullOpts,
    budget: usize,
    may_cut: bool,
) -> Result<(String, usize)> {
    let rules = load_rules(tx, name)?;
    let state: Option<(i64, i64, i64, i64)> = tx
        .query_row(
            "SELECT origin, cursor, dropped, errors FROM subscribers
             WHERE subscription = ?1 AND subscriber = ?2",
            params![name, subscriber],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((origin, cursor, s_dropped, s_errors)) = state else {
        return Err(Error::NotSubscribed {
            subscription: name.into(),
            subscriber: subscriber.into(),
        });
    };
    let filter = rules.filter.as_deref().map(Expr::compile).transpose()?;
    let template = rules.template.as_deref().map(Expr::compile).transpose()?;
    let (lwm, p_dropped, p_errors, p_parked): (i64, i64, i64, i64) = tx.query_row(
        "SELECT lwm, dropped, errors, parked FROM subscriptions WHERE name = ?1",
        [name],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let from = match rules.share {
        Share::All => cursor,
        Share::One => lwm,
    };
    let latest = rules.select == Select::Latest;
    let rows = scan(tx, &rules.queue, from, latest)?;
    let window_full = rows.len() as i64 == SCAN;
    let head = if latest {
        rows.first().map(|r| r.id)
    } else {
        None
    };

    // Rows the filter failed on, and parked rows. Each counts once, in
    // the pull whose committed position moves past it.
    let mut errors: Vec<(i64, String)> = Vec::new();
    let mut parked: Vec<i64> = Vec::new();
    let mut matched: Vec<bool> = Vec::with_capacity(rows.len());
    let mut candidates: Vec<&Row> = Vec::new();
    for row in &rows {
        let ok = match &filter {
            None => true,
            Some(f) => match f.matches(&row.value()?) {
                Ok(b) => b,
                Err(e) => {
                    errors.push((row.id, e.to_string()));
                    false
                }
            },
        };
        matched.push(ok);
        if !ok {
            continue;
        }
        if rules.share == Share::One {
            if is_acked(tx, name, row.id)? {
                continue;
            }
            if let Some((_, until, attempts)) = lease_of(tx, name, row.id)? {
                if until > opts.now {
                    continue;
                }
                if attempts >= rules.max_attempts {
                    parked.push(row.id);
                    continue;
                }
            }
        }
        candidates.push(row);
        if latest && candidates.len() == rules.limit.unwrap_or(usize::MAX) {
            break;
        }
    }
    // `latest` candidates are newest first, so the budget keeps the
    // newest; the lines are printed oldest first below.

    // Reserve the longest status line these names can print, then fit
    // items in the rest.
    let room = budget.saturating_sub(status_max(name.len(), subscriber.len()));
    let tagged = rules.consume == Consume::OnAck;
    let limit = rules.limit.unwrap_or(usize::MAX);
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0;
    let mut delivered: Vec<i64> = Vec::new();
    let mut stopped = false;
    for row in &candidates {
        if delivered.len() == limit {
            stopped = true;
            break;
        }
        let mut line = render(template.as_ref(), row, tagged)?;
        line.push('\n');
        if used + line.len() > room {
            if delivered.is_empty() && may_cut && room >= ITEM_SLICE {
                line = cut(&line, room, row.id);
            } else {
                stopped = true;
                break;
            }
        }
        used += line.len();
        lines.push(line);
        delivered.push(row.id);
    }
    if latest {
        lines.reverse();
        delivered.reverse();
    }
    // A budget stop before any delivery: the position stays, so the
    // items wait for a pull with room.
    let starved = stopped && delivered.is_empty();
    let mut status = Status {
        delivered: delivered.len(),
        // A `latest` pull that delivered skips older items by design,
        // so a budget stop there has nothing to pull again.
        more: starved || (!latest && (stopped || window_full)),
        ..Status::default()
    };
    if !delivered.is_empty() && rules.consume == Consume::OnAck {
        match rules.share {
            Share::All => {
                status.unacked = count_open(tx, name, &rules, filter.as_ref(), origin, cursor)?;
            }
            Share::One => status.lease = rules.lease,
        }
    }

    // Record the delivery, and the position it commits.
    let position = match rules.share {
        Share::All => {
            let new_cursor = if latest && !starved {
                head.unwrap_or(cursor)
            } else if stopped {
                delivered.last().copied().unwrap_or(cursor)
            } else {
                rows.last().map_or(cursor, |r| r.id)
            };
            tx.execute(
                "UPDATE subscribers SET cursor = ?3 WHERE subscription = ?1 AND subscriber = ?2",
                params![name, subscriber, new_cursor],
            )?;
            new_cursor
        }
        Share::One => {
            for id in &delivered {
                match rules.consume {
                    Consume::OnRead => {
                        tx.execute(
                            "INSERT OR IGNORE INTO acks(subscription, item, owner, acked_at)
                             VALUES (?1, ?2, ?3, ?4)",
                            params![name, id, subscriber, opts.now],
                        )?;
                    }
                    Consume::OnAck => {
                        tx.execute(
                            "INSERT INTO leases(subscription, item, owner, expires_at, attempts)
                             VALUES (?1, ?2, ?3, ?4, 1)
                             ON CONFLICT(subscription, item) DO UPDATE SET owner = excluded.owner,
                               expires_at = excluded.expires_at, attempts = attempts + 1",
                            params![name, id, subscriber, opts.now + rules.lease],
                        )?;
                    }
                }
            }
            let new_lwm = advance_lwm(tx, name, &rules, from, &rows, &matched, opts.now)?;
            tx.execute(
                "UPDATE subscriptions SET lwm = ?2 WHERE name = ?1",
                params![name, new_lwm],
            )?;
            new_lwm
        }
    };

    // Count what the committed position moved past, report the stored
    // counts with it, and zero them in the same commit.
    let passed: Vec<&(i64, String)> = errors.iter().filter(|(id, _)| *id <= position).collect();
    let last_error = passed.iter().max_by_key(|(id, _)| *id).map(|(_, e)| e);
    let new_errors = passed.len() as i64;
    match rules.share {
        Share::All => {
            status.errors = s_errors + new_errors;
            status.dropped = s_dropped;
            tx.execute(
                "UPDATE subscribers SET errors = 0, dropped = 0, last_error = coalesce(?3, last_error)
                 WHERE subscription = ?1 AND subscriber = ?2",
                params![name, subscriber, last_error],
            )?;
        }
        Share::One => {
            status.errors = p_errors + new_errors;
            status.dropped = p_dropped;
            status.parked = p_parked + parked.iter().filter(|id| **id <= position).count() as i64;
            tx.execute(
                "UPDATE subscriptions SET errors = 0, dropped = 0, parked = 0,
                   last_error = coalesce(?2, last_error)
                 WHERE name = ?1",
                params![name, last_error],
            )?;
        }
    }
    if !delivered.is_empty() {
        tx.execute(
            "UPDATE subscribers SET next_at = ?3 WHERE subscription = ?1 AND subscriber = ?2",
            params![name, subscriber, opts.now + rules.every.unwrap_or(0)],
        )?;
    }
    if !status.reports() {
        return Ok((String::new(), 0));
    }
    let text = status.line(name, subscriber) + &lines.concat();
    Ok((text, delivered.len()))
}

/// The template's text, or the plain form when the template yields
/// nothing or fails on this item. A `tagged` line (an on-ack
/// subscription's) starts with `[queue#id] `, so an agent can ack it
/// under any template.
fn render(template: Option<&Expr>, row: &Row, tagged: bool) -> Result<String> {
    let Some(t) = template else {
        return Ok(row.plain());
    };
    Ok(match t.render(&row.value()?) {
        Ok(s) if !s.is_empty() && tagged => format!("[{}#{}] {s}", row.queue, row.id),
        Ok(s) if !s.is_empty() => s,
        Ok(_) => row.plain(),
        Err(e) => format!("{} (template error: {e})", row.plain()),
    })
}

fn start_position(tx: &Connection, rules: &Rules) -> Result<i64> {
    Ok(match rules.start {
        Start::All => 0,
        Start::New => tx.query_row(
            "SELECT coalesce(max(id), 0) FROM items WHERE queue = ?1",
            [&rules.queue],
            |r| r.get(0),
        )?,
    })
}

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        queue: r.get(1)?,
        key: r.get(2)?,
        payload: r.get(3)?,
        created_at: r.get(4)?,
    })
}

fn item(conn: &Connection, r: Row) -> Result<Item> {
    let mut st = conn.prepare_cached(
        "SELECT subscription, owner, acked_at FROM acks WHERE item = ?1 ORDER BY subscription",
    )?;
    let acks = st
        .query_map([r.id], |a| {
            Ok(Ack {
                subscription: a.get(0)?,
                owner: a.get(1)?,
                at: a.get(2)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(Item {
        id: r.id,
        queue: r.queue,
        key: r.key,
        payload: serde_json::from_str(&r.payload)?,
        created_at: r.created_at,
        acks,
    })
}

fn load_rules(tx: &Connection, name: &str) -> Result<Rules> {
    let text: Option<String> = tx
        .query_row(
            "SELECT rules FROM subscriptions WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    let text = text.ok_or_else(|| Error::NoSubscription(name.into()))?;
    Ok(serde_json::from_str(&text)?)
}

/// Rows after `after`: oldest first, or newest first for `latest`.
fn scan(tx: &Connection, queue: &str, after: i64, newest_first: bool) -> Result<Vec<Row>> {
    let sql = if newest_first {
        "SELECT id, queue, key, payload, created_at FROM items
         WHERE queue = ?1 AND id > ?2 ORDER BY id DESC LIMIT ?3"
    } else {
        "SELECT id, queue, key, payload, created_at FROM items
         WHERE queue = ?1 AND id > ?2 ORDER BY id LIMIT ?3"
    };
    let mut st = tx.prepare_cached(sql)?;
    let rows = st.query_map(params![queue, after, SCAN], row_from)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn is_acked(tx: &Connection, sub: &str, id: i64) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM acks WHERE subscription = ?1 AND item = ?2)",
        params![sub, id],
        |r| r.get(0),
    )?)
}

fn lease_of(tx: &Connection, sub: &str, id: i64) -> Result<Option<(String, i64, i64)>> {
    Ok(tx
        .query_row(
            "SELECT owner, expires_at, attempts FROM leases WHERE subscription = ?1 AND item = ?2",
            params![sub, id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

/// The share: one low-water mark moves past leading rows that are done:
/// filtered out, acked, or parked. A live lease holds it.
fn advance_lwm(
    tx: &Connection,
    sub: &str,
    rules: &Rules,
    from: i64,
    rows: &[Row],
    matched: &[bool],
    now: i64,
) -> Result<i64> {
    let mut lwm = from;
    for (row, ok) in rows.iter().zip(matched) {
        let parked = rules.consume == Consume::OnAck
            && matches!(lease_of(tx, sub, row.id)?,
                Some((_, until, a)) if a >= rules.max_attempts && until <= now);
        if !ok || is_acked(tx, sub, row.id)? || parked {
            lwm = row.id;
        } else {
            break;
        }
    }
    Ok(lwm)
}

/// Items this subscriber was shown in earlier pulls and nobody in the
/// subscription has acked.
fn count_open(
    tx: &Connection,
    sub: &str,
    rules: &Rules,
    filter: Option<&Expr>,
    origin: i64,
    cursor: i64,
) -> Result<i64> {
    let mut st = tx.prepare_cached(
        "SELECT id, queue, key, payload, created_at FROM items i
         WHERE queue = ?1 AND id > ?2 AND id <= ?3
           AND NOT EXISTS (SELECT 1 FROM acks a WHERE a.subscription = ?4 AND a.item = i.id)
         ORDER BY id DESC LIMIT ?5",
    )?;
    let rows: Vec<Row> = st
        .query_map(params![rules.queue, origin, cursor, sub, SCAN], row_from)?
        .collect::<std::result::Result<_, _>>()?;
    let mut n = 0;
    for row in rows {
        if let Some(f) = filter
            && !f.matches(&row.value()?).unwrap_or(false)
        {
            continue;
        }
        n += 1;
    }
    Ok(n)
}

/// Cut a rendered item to `room` bytes, ending with a whole pointer to
/// the whole item. `room` is at least `ITEM_SLICE`, which holds the
/// longest pointer.
fn cut(line: &str, room: usize, id: i64) -> String {
    let tail = format!(" … tundish show {id}\n");
    let mut end = room.saturating_sub(tail.len()).min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{tail}", &line[..end])
}

/// Drop old items. `max_items` never trims past the lowest unread
/// position of any subscription; `max_age` trims regardless and counts,
/// per subscriber, the dropped items its filter would have delivered.
fn trim(tx: &Connection, queue: &str, now: i64, r: Retention) -> Result<()> {
    let mut cutoff = 0_i64;
    if let Some(max) = r.max_items {
        let by_count: i64 = tx.query_row(
            "SELECT coalesce((SELECT id FROM items WHERE queue = ?1 ORDER BY id DESC LIMIT 1 OFFSET ?2), 0)",
            params![queue, max],
            |row| row.get(0),
        )?;
        let floor: Option<i64> = tx.query_row(
            "SELECT min(pos) FROM (
               SELECT s.cursor AS pos FROM subscribers s JOIN subscriptions p ON p.name = s.subscription
                 WHERE p.queue = ?1 AND json_extract(p.rules, '$.share') IS NOT 'one'
               UNION ALL
               SELECT p.lwm FROM subscriptions p WHERE p.queue = ?1 AND json_extract(p.rules, '$.share') = 'one'
                 AND EXISTS (SELECT 1 FROM subscribers s WHERE s.subscription = p.name))",
            [queue],
            |row| row.get(0),
        )?;
        cutoff = cutoff.max(floor.map_or(by_count, |f| by_count.min(f)));
    }
    if let Some(age) = r.max_age {
        let by_age: i64 = tx.query_row(
            "SELECT coalesce(max(id), 0) FROM items WHERE queue = ?1 AND created_at < ?2",
            params![queue, now - age],
            |row| row.get(0),
        )?;
        cutoff = cutoff.max(by_age);
    }
    if cutoff == 0 {
        return Ok(());
    }
    count_drops(tx, queue, cutoff)?;
    for table in ["acks", "leases"] {
        tx.execute(
            &format!(
                "DELETE FROM {table} WHERE item IN (SELECT id FROM items WHERE queue = ?1 AND id <= ?2)"
            ),
            params![queue, cutoff],
        )?;
    }
    tx.execute(
        "DELETE FROM items WHERE queue = ?1 AND id <= ?2",
        params![queue, cutoff],
    )?;
    Ok(())
}

/// Before a trim to `cutoff`, count the items each position has not
/// received that its filter would deliver, and move the position to the
/// cutoff. A `share: one` subscription has one position and one count,
/// on its own row; a `share: all` subscriber has its own.
fn count_drops(tx: &Connection, queue: &str, cutoff: i64) -> Result<()> {
    let mut st = tx.prepare(
        "SELECT p.name, p.rules, p.lwm, s.subscriber, s.cursor
         FROM subscriptions p JOIN subscribers s ON s.subscription = p.name WHERE p.queue = ?1
         ORDER BY p.name, s.subscriber",
    )?;
    let subs: Vec<(String, String, i64, String, i64)> = st
        .query_map([queue], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    drop(st);
    let mut counted: Option<String> = None;
    for (name, rules, lwm, subscriber, cursor) in subs {
        let rules: Rules = serde_json::from_str(&rules)?;
        let one = rules.share == Share::One;
        let pos = if one { lwm } else { cursor };
        if pos >= cutoff {
            continue;
        }
        if one && counted.as_deref() == Some(name.as_str()) {
            // The subscription's count is in; move this subscriber too.
            tx.execute(
                "UPDATE subscribers SET cursor = max(cursor, ?3)
                 WHERE subscription = ?1 AND subscriber = ?2",
                params![name, subscriber, cutoff],
            )?;
            continue;
        }
        let filter = rules.filter.as_deref().map(Expr::compile).transpose()?;
        let mut q = tx.prepare_cached(
            "SELECT id, queue, key, payload, created_at FROM items i
             WHERE queue = ?1 AND id > ?2 AND id <= ?3
               AND NOT EXISTS (SELECT 1 FROM acks a WHERE a.subscription = ?4 AND a.item = i.id)",
        )?;
        let rows: Vec<Row> = q
            .query_map(params![queue, pos, cutoff, name], row_from)?
            .collect::<std::result::Result<_, _>>()?;
        let mut lost = 0_i64;
        for row in rows {
            let keep = match &filter {
                None => true,
                Some(f) => f.matches(&row.value()?).unwrap_or(false),
            };
            if keep {
                lost += 1;
            }
        }
        if one {
            // The lwm moves after the loop, so each row here reads the
            // old one; `counted` keeps the count to one per subscription.
            tx.execute(
                "UPDATE subscriptions SET dropped = dropped + ?2 WHERE name = ?1",
                params![name, lost],
            )?;
            tx.execute(
                "UPDATE subscribers SET cursor = max(cursor, ?3)
                 WHERE subscription = ?1 AND subscriber = ?2",
                params![name, subscriber, cutoff],
            )?;
            counted = Some(name);
        } else {
            tx.execute(
                "UPDATE subscribers SET dropped = dropped + ?3, cursor = max(cursor, ?4)
                 WHERE subscription = ?1 AND subscriber = ?2",
                params![name, subscriber, lost, cutoff],
            )?;
        }
    }
    tx.execute(
        "UPDATE subscriptions SET lwm = max(lwm, ?2)
         WHERE queue = ?1 AND json_extract(rules, '$.share') = 'one'
           AND EXISTS (SELECT 1 FROM subscribers s WHERE s.subscription = name)",
        params![queue, cutoff],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_max_is_the_longest_line_the_formatter_prints() {
        let name = "s".repeat(NAME_MAX);
        let who = "w".repeat(NAME_MAX);
        let max = Status {
            delivered: i64::MAX as usize,
            more: true,
            unacked: i64::MAX,
            parked: i64::MAX,
            errors: i64::MAX,
            dropped: i64::MAX,
            lease: i64::MAX,
        };
        assert_eq!(max.line(&name, &who).len(), STATUS_MAX);
        assert_eq!(STATUS_MAX, 336);
        assert_eq!(MIN_BYTES, 400);
    }

    #[test]
    fn the_longest_pointer_fits_an_item_slice() {
        let line = "x".repeat(1000);
        let cut = cut(&line, ITEM_SLICE, i64::MAX);
        assert_eq!(cut.len(), ITEM_SLICE);
        assert!(cut.ends_with(&format!(" … tundish show {}\n", i64::MAX)));
    }
}
