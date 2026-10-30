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
  created_at INTEGER NOT NULL
);
CREATE TABLE subscribers (
  subscription TEXT NOT NULL REFERENCES subscriptions(name) ON DELETE CASCADE,
  subscriber TEXT NOT NULL,
  origin INTEGER NOT NULL,
  cursor INTEGER NOT NULL,
  next_at INTEGER NOT NULL DEFAULT 0,
  dropped INTEGER NOT NULL DEFAULT 0,
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

/// The smallest byte budget a pull accepts.
pub const MIN_BYTES: usize = 128;

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
    /// The budget for the whole printed batch, footers included.
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
        if subscriber.is_empty() || name.is_empty() {
            return Err(Error::Usage(
                "a subscription and a subscriber need names".into(),
            ));
        }
        let tx = self.write()?;
        let mut change = Change::None;
        if let Some(r) = &rules {
            r.check()?;
            // A share one subscription has one position for all its
            // subscribers, fixed when it is created or moved.
            let start = start_position(&tx, r)?;
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
            if matches!(change, Change::Moved { .. }) {
                tx.execute(
                    "UPDATE subscriptions SET lwm = ?2 WHERE name = ?1",
                    params![name, start],
                )?;
                tx.execute("DELETE FROM acks WHERE subscription = ?1", [name])?;
                tx.execute("DELETE FROM leases WHERE subscription = ?1", [name])?;
                tx.execute(
                    "UPDATE subscribers SET origin = ?2, cursor = ?2, dropped = 0 WHERE subscription = ?1",
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
        let (text, n) = pull_in(&tx, name, subscriber, opts, opts.max_bytes, true, false)?;
        finish(tx, &text, opts, out)?;
        Ok(n)
    }

    /// Pull every due subscription of `subscriber` in one transaction,
    /// sharing one byte budget: all of it is marked delivered, or none.
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
        let names = self.due(subscriber, opts.now)?;
        let tx = self.write()?;
        let mut text = String::new();
        let mut n = 0;
        for name in names {
            let left = opts.max_bytes.saturating_sub(text.len());
            if left < MIN_BYTES / 2 {
                break;
            }
            let (t, k) = pull_in(&tx, &name, subscriber, opts, left, text.is_empty(), true)?;
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

    /// Every subscriber's position. For a `share: one` subscription the
    /// position and the unread count are the subscription's.
    pub fn status(&self) -> Result<Vec<SubscriberState>> {
        let mut st = self.conn.prepare(
            "SELECT s.subscription, s.subscriber, p.queue,
               CASE WHEN json_extract(p.rules, '$.share') = 'one' THEN p.lwm ELSE s.cursor END AS pos,
               s.next_at, s.dropped,
               CASE WHEN json_extract(p.rules, '$.share') = 'one'
                 THEN (SELECT count(*) FROM items i WHERE i.queue = p.queue AND i.id > p.lwm
                         AND NOT EXISTS (SELECT 1 FROM acks a WHERE a.subscription = p.name AND a.item = i.id))
                 ELSE (SELECT count(*) FROM items i WHERE i.queue = p.queue AND i.id > s.cursor) END
             FROM subscribers s JOIN subscriptions p ON p.name = s.subscription
             ORDER BY s.subscription, s.subscriber",
        )?;
        let rows = st.query_map([], |r| {
            Ok(SubscriberState {
                subscription: r.get(0)?,
                subscriber: r.get(1)?,
                queue: r.get(2)?,
                cursor: r.get(3)?,
                next_at: r.get(4)?,
                dropped: r.get(5)?,
                unread: r.get(6)?,
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

/// What one pull found besides the items it printed.
#[derive(Default)]
struct Notes {
    open: i64,
    parked: i64,
    filter_errors: i64,
    first_error: Option<String>,
    dropped: i64,
}

fn footer(
    rules: &Rules,
    name: &str,
    subscriber: &str,
    notes: &Notes,
    delivered: bool,
    more: bool,
) -> String {
    let mut f = String::new();
    if delivered && rules.consume == Consume::OnAck {
        f.push_str(&format!(
            "ack: tundish ack <id> --as {subscriber} --subscription {name}"
        ));
        if rules.share == Share::One {
            f.push_str(&format!(" (leases end in {}s)", rules.lease));
        }
        f.push('\n');
    }
    if notes.open > 0 {
        f.push_str(&format!(
            "{} unacked in {name} from earlier pulls: tundish list {} --unacked --subscription {name}\n",
            notes.open, rules.queue
        ));
    }
    if notes.parked > 0 {
        f.push_str(&format!(
            "{} parked in {name} after {} attempts: tundish list {} --unacked --subscription {name}\n",
            notes.parked, rules.max_attempts, rules.queue
        ));
    }
    if notes.filter_errors > 0 {
        f.push_str(&format!(
            "{} {} in {name} failed the filter: {}\n",
            notes.filter_errors,
            if notes.filter_errors == 1 {
                "item"
            } else {
                "items"
            },
            notes.first_error.as_deref().unwrap_or("")
        ));
    }
    if notes.dropped > 0 {
        f.push_str(&if notes.dropped == 1 {
            format!("1 unread item in {name} was dropped by retention\n")
        } else {
            format!(
                "{} unread items in {name} were dropped by retention\n",
                notes.dropped
            )
        });
    }
    if more {
        f.push_str(&format!("more in {name}: pull again\n"));
    }
    f
}

/// One subscription's batch, recorded in `tx`, returned as text that
/// fits `budget` with its footer. Only the first item of a whole
/// output (`may_cut`) is cut to fit. A `quiet` pull that delivers no
/// item prints only the notices that happen once, so a hook does not
/// repeat the unacked and parked counts.
fn pull_in(
    tx: &Connection,
    name: &str,
    subscriber: &str,
    opts: &PullOpts,
    budget: usize,
    may_cut: bool,
    quiet: bool,
) -> Result<(String, usize)> {
    let rules = load_rules(tx, name)?;
    let state: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT origin, cursor, dropped FROM subscribers
             WHERE subscription = ?1 AND subscriber = ?2",
            params![name, subscriber],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((origin, cursor, dropped)) = state else {
        return Err(Error::NotSubscribed {
            subscription: name.into(),
            subscriber: subscriber.into(),
        });
    };
    let filter = rules.filter.as_deref().map(Expr::compile).transpose()?;
    let template = rules.template.as_deref().map(Expr::compile).transpose()?;
    let lwm: i64 = tx.query_row(
        "SELECT lwm FROM subscriptions WHERE name = ?1",
        [name],
        |r| r.get(0),
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

    let mut notes = Notes {
        dropped,
        ..Notes::default()
    };
    let mut matched: Vec<bool> = Vec::with_capacity(rows.len());
    let mut candidates: Vec<&Row> = Vec::new();
    for row in &rows {
        let ok = match &filter {
            None => true,
            Some(f) => match f.matches(&row.value()?) {
                Ok(b) => b,
                Err(e) => {
                    notes.filter_errors += 1;
                    notes.first_error.get_or_insert_with(|| e.to_string());
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
                    notes.parked += 1;
                    continue;
                }
            }
        }
        candidates.push(row);
        if latest && candidates.len() == rules.limit.unwrap_or(usize::MAX) {
            break;
        }
    }
    if latest {
        candidates.reverse();
    }
    if rules.consume == Consume::OnAck && rules.share == Share::All {
        notes.open = count_open(tx, name, &rules, filter.as_ref(), origin, cursor)?;
    }

    // Reserve the largest footer this pull can print, then fit items.
    let reserve = footer(&rules, name, subscriber, &notes, true, true).len();
    let room = budget.saturating_sub(reserve);
    let limit = rules.limit.unwrap_or(usize::MAX);
    let mut text = String::new();
    let mut delivered: Vec<i64> = Vec::new();
    let mut stopped = false;
    for row in &candidates {
        if delivered.len() == limit {
            stopped = true;
            break;
        }
        let mut line = render(template.as_ref(), row)?;
        line.push('\n');
        if text.len() + line.len() > room {
            if delivered.is_empty() && may_cut && room > 0 {
                line = cut(&line, room, row.id);
            } else {
                stopped = true;
                break;
            }
        }
        text.push_str(&line);
        delivered.push(row.id);
    }
    let more = stopped || (window_full && !latest);

    // Record the delivery.
    match rules.share {
        Share::All => {
            let new_cursor = if latest {
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
        }
    }
    if quiet && delivered.is_empty() {
        // The open and parked counts repeat on every pull; filter errors
        // and drops are reported once, so they still print.
        notes.open = 0;
        notes.parked = 0;
        if notes.filter_errors == 0 && notes.dropped == 0 {
            return Ok((String::new(), 0));
        }
    }
    if dropped > 0 {
        tx.execute(
            "UPDATE subscribers SET dropped = 0 WHERE subscription = ?1 AND subscriber = ?2",
            params![name, subscriber],
        )?;
    }
    if !delivered.is_empty() {
        tx.execute(
            "UPDATE subscribers SET next_at = ?3 WHERE subscription = ?1 AND subscriber = ?2",
            params![name, subscriber, opts.now + rules.every.unwrap_or(0)],
        )?;
    }
    text.push_str(&footer(
        &rules,
        name,
        subscriber,
        &notes,
        !delivered.is_empty(),
        more,
    ));
    Ok((text, delivered.len()))
}

/// The template's text, or the plain form when the template yields
/// nothing or fails on this item.
fn render(template: Option<&Expr>, row: &Row) -> Result<String> {
    let Some(t) = template else {
        return Ok(row.plain());
    };
    Ok(match t.render(&row.value()?) {
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

/// Cut a rendered item to `room` bytes, ending with a pointer to the
/// whole item. The pointer itself is cut when `room` is smaller.
fn cut(line: &str, room: usize, id: i64) -> String {
    let tail = format!(" … tundish show {id}\n");
    if room <= tail.len() {
        let mut end = room.saturating_sub(1);
        while !tail.is_char_boundary(end) {
            end -= 1;
        }
        return format!("{}\n", &tail[..end]);
    }
    let mut end = (room - tail.len()).min(line.len());
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

/// Before a trim to `cutoff`, add to each subscriber's `dropped` the
/// items it has not received that its filter would deliver, and move
/// its position to the cutoff.
fn count_drops(tx: &Connection, queue: &str, cutoff: i64) -> Result<()> {
    let mut st = tx.prepare(
        "SELECT p.name, p.rules, p.lwm, s.subscriber, s.cursor
         FROM subscriptions p JOIN subscribers s ON s.subscription = p.name WHERE p.queue = ?1",
    )?;
    let subs: Vec<(String, String, i64, String, i64)> = st
        .query_map([queue], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    drop(st);
    for (name, rules, lwm, subscriber, cursor) in subs {
        let rules: Rules = serde_json::from_str(&rules)?;
        let pos = if rules.share == Share::One {
            lwm
        } else {
            cursor
        };
        if pos >= cutoff {
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
        tx.execute(
            "UPDATE subscribers SET dropped = dropped + ?3, cursor = max(cursor, ?4)
             WHERE subscription = ?1 AND subscriber = ?2",
            params![name, subscriber, lost, cutoff],
        )?;
        if rules.share == Share::One {
            tx.execute(
                "UPDATE subscriptions SET lwm = max(lwm, ?2) WHERE name = ?1",
                params![name, cutoff],
            )?;
        }
    }
    Ok(())
}
