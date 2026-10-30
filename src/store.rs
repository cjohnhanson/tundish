//! The SQLite store: one file, one schema, every write in one
//! `BEGIN IMMEDIATE` transaction.

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
  item INTEGER PRIMARY KEY,
  owner TEXT NOT NULL,
  acked_at INTEGER NOT NULL
);
CREATE TABLE leases (
  item INTEGER PRIMARY KEY,
  owner TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  attempts INTEGER NOT NULL
);
";

/// Rows scanned per pull, so a pull holds the write lock briefly.
const SCAN: i64 = 1000;

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
    /// The budget for the whole printed batch.
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

/// One stored item.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: i64,
    pub queue: String,
    pub key: Option<String>,
    pub payload: Value,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acked_by: Option<String>,
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
        let key = serde_json::to_string(&self.key)?;
        expr::value(&format!(
            r#"{{"id":{},"queue":{},"key":{},"payload":{},"created_at":{}}}"#,
            self.id,
            serde_json::to_string(&self.queue)?,
            key,
            self.payload,
            self.created_at
        ))
    }
}

const DEFAULT_TEMPLATE: &str =
    r#""[\(.queue)#\(.id)]" + (if .key then " \(.key)" else "" end) + " \(.payload | tojson)""#;

impl Store {
    /// Open or create the store at `path`, waiting up to five seconds
    /// for another writer.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, Duration::from_secs(5))
    }

    /// Open with an explicit wait for the write lock.
    pub fn open_with(path: &Path, busy: Duration) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(busy)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    fn migrate(&mut self) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let v: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(Error::Usage(format!(
                "this database has schema {v}; this tundish knows {SCHEMA_VERSION}. Upgrade tundish."
            )));
        }
        if v == 0 {
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        tx.commit()?;
        Ok(())
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
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
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
    /// `subscriber` to it. Given rules replace the stored ones.
    pub fn subscribe(
        &mut self,
        name: &str,
        subscriber: &str,
        rules: Option<Rules>,
        now: i64,
    ) -> Result<()> {
        if subscriber.is_empty() {
            return Err(Error::Usage("a subscriber name cannot be empty".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(r) = &rules {
            r.check()?;
            tx.execute(
                "INSERT INTO subscriptions(name, queue, rules, lwm, created_at)
                 VALUES (?1, ?2, ?3, 0, ?4)
                 ON CONFLICT(name) DO UPDATE SET queue = excluded.queue, rules = excluded.rules",
                params![name, r.queue, serde_json::to_string(r)?, now],
            )?;
        }
        let rules = load_rules(&tx, name)?;
        let head: i64 = tx.query_row(
            "SELECT coalesce(max(id), 0) FROM items WHERE queue = ?1",
            [&rules.queue],
            |r| r.get(0),
        )?;
        let origin = match rules.start {
            Start::All => 0,
            Start::New => head,
        };
        tx.execute(
            "INSERT OR IGNORE INTO subscribers(subscription, subscriber, origin, cursor)
             VALUES (?1, ?2, ?3, ?3)",
            params![name, subscriber, origin],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Unbind a subscriber; the last one out leaves the subscription.
    pub fn unsubscribe(&mut self, name: &str, subscriber: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM subscribers WHERE subscription = ?1 AND subscriber = ?2",
            params![name, subscriber],
        )?;
        Ok(n > 0)
    }

    /// The subscriptions `subscriber` is bound to whose `every` window
    /// has passed, in name order.
    pub fn due(&self, subscriber: &str, now: i64) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT subscription FROM subscribers WHERE subscriber = ?1 AND next_at <= ?2
             ORDER BY subscription",
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
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (text, n) = pull_in(&tx, name, subscriber, opts, opts.max_bytes)?;
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
        let names = self.due(subscriber, opts.now)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut text = String::new();
        let mut n = 0;
        for name in names {
            let left = opts.max_bytes.saturating_sub(text.len());
            if left == 0 {
                break;
            }
            let (t, k) = pull_in(&tx, &name, subscriber, opts, left)?;
            text.push_str(&t);
            n += k;
        }
        finish(tx, &text, opts, out)?;
        Ok(n)
    }
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

/// One subscription's batch, recorded in `tx`, returned as text.
fn pull_in(
    tx: &Connection,
    name: &str,
    subscriber: &str,
    opts: &PullOpts,
    budget: usize,
) -> Result<(String, usize)> {
    {
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
        let template = Expr::compile(rules.template.as_deref().unwrap_or(DEFAULT_TEMPLATE))?;
        let lwm: i64 = tx.query_row(
            "SELECT lwm FROM subscriptions WHERE name = ?1",
            [name],
            |r| r.get(0),
        )?;
        let from = match rules.share {
            Share::All => cursor,
            Share::One => lwm.max(origin),
        };
        let rows = scan(tx, &rules.queue, from)?;
        let scanned_to = rows.last().map(|r| r.id);

        // Candidates: matching rows this subscriber may take.
        let mut candidates = Vec::new();
        let mut parked = 0;
        for row in &rows {
            if let Some(f) = &filter
                && !f.matches(&row.value()?)?
            {
                continue;
            }
            if rules.share == Share::One {
                if is_acked(tx, row.id)? {
                    continue;
                }
                match lease_of(tx, row.id)? {
                    Some((owner, until, _)) if until > opts.now && owner != subscriber => continue,
                    Some((_, until, attempts))
                        if until <= opts.now && attempts >= rules.max_attempts =>
                    {
                        parked += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            candidates.push(row);
        }
        if rules.select == Select::Latest {
            let keep = rules.limit.unwrap_or(usize::MAX);
            let skip = candidates.len().saturating_sub(keep);
            candidates.drain(..skip);
        }

        // Render until the limit or the byte budget.
        let mut text = String::new();
        let mut delivered: Vec<i64> = Vec::new();
        let limit = rules.limit.unwrap_or(usize::MAX);
        let mut stopped_early = false;
        for row in &candidates {
            if delivered.len() == limit {
                stopped_early = true;
                break;
            }
            let mut line = template.render(&row.value()?)?;
            line.push('\n');
            let room = budget.saturating_sub(text.len());
            if line.len() > room {
                if delivered.is_empty() {
                    line = cut(&line, room, row.id);
                } else {
                    stopped_early = true;
                    break;
                }
            }
            text.push_str(&line);
            delivered.push(row.id);
        }

        // Record the delivery.
        let last = delivered.last().copied();
        match rules.share {
            Share::All => {
                let new_cursor = if stopped_early {
                    last.unwrap_or(cursor)
                } else {
                    scanned_to.unwrap_or(cursor)
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
                                "INSERT OR IGNORE INTO acks(item, owner, acked_at) VALUES (?1, ?2, ?3)",
                                params![id, subscriber, opts.now],
                            )?;
                        }
                        Consume::OnAck => {
                            tx.execute(
                                "INSERT INTO leases(item, owner, expires_at, attempts) VALUES (?1, ?2, ?3, 1)
                                 ON CONFLICT(item) DO UPDATE SET owner = excluded.owner,
                                   expires_at = excluded.expires_at, attempts = attempts + 1",
                                params![id, subscriber, opts.now + rules.lease],
                            )?;
                        }
                    }
                }
                let new_lwm = advance_lwm(tx, &rules, filter.as_ref(), from, &rows, opts.now)?;
                tx.execute(
                    "UPDATE subscriptions SET lwm = ?2 WHERE name = ?1",
                    params![name, new_lwm],
                )?;
            }
        }

        // Footer lines: open acks, parked items, forced drops.
        if rules.consume == Consume::OnAck && rules.share == Share::All {
            if !delivered.is_empty() {
                text.push_str(&format!("ack: tundish ack <id> --as {subscriber}\n"));
            }
            let new_cursor: i64 = tx.query_row(
                "SELECT cursor FROM subscribers WHERE subscription = ?1 AND subscriber = ?2",
                params![name, subscriber],
                |r| r.get(0),
            )?;
            let open = count_open(tx, &rules, filter.as_ref(), origin, new_cursor, &delivered)?;
            if open > 0 {
                text.push_str(&format!(
                    "{open} unacked in {name} from earlier pulls: tundish list {} --unacked\n",
                    rules.queue
                ));
            }
        }
        if rules.share == Share::One && rules.consume == Consume::OnAck && !delivered.is_empty() {
            text.push_str(&format!(
                "ack: tundish ack <id> --as {subscriber} (leases end in {}s)\n",
                rules.lease
            ));
        }
        if parked > 0 {
            text.push_str(&format!(
                "{parked} parked in {name} after {} attempts: tundish list {}\n",
                rules.max_attempts, rules.queue
            ));
        }
        if dropped > 0 {
            text.push_str(&format!(
                "{dropped} unread items in {name} were dropped by retention\n"
            ));
            tx.execute(
                "UPDATE subscribers SET dropped = 0 WHERE subscription = ?1 AND subscriber = ?2",
                params![name, subscriber],
            )?;
        }
        if !delivered.is_empty()
            && let Some(every) = rules.every
        {
            tx.execute(
                "UPDATE subscribers SET next_at = ?3 WHERE subscription = ?1 AND subscriber = ?2",
                params![name, subscriber, opts.now + every],
            )?;
        }

        Ok((text, delivered.len()))
    }
}

impl Store {
    /// Ack an item for `owner`. The first ack wins; a repeat by the
    /// same owner is a no-op that still reports `Acked`.
    pub fn ack(&mut self, item: i64, owner: &str, now: i64) -> Result<AckOutcome> {
        if owner.is_empty() {
            return Err(Error::Usage("an ack needs an owner".into()));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE id = ?1)",
            [item],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(AckOutcome::NotFound);
        }
        if let Some((holder, until, _)) = lease_of(&tx, item)?
            && holder != owner
            && until > now
        {
            return Ok(AckOutcome::LeasedBy {
                owner: holder,
                until,
            });
        }
        let won: Option<i64> = tx
            .query_row(
                "INSERT INTO acks(item, owner, acked_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(item) DO NOTHING RETURNING item",
                params![item, owner, now],
                |r| r.get(0),
            )
            .optional()?;
        let outcome = if won.is_some() {
            AckOutcome::Acked
        } else {
            let (holder, at): (String, i64) = tx.query_row(
                "SELECT owner, acked_at FROM acks WHERE item = ?1",
                [item],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if holder == owner {
                AckOutcome::Acked
            } else {
                AckOutcome::AlreadyAcked { owner: holder, at }
            }
        };
        tx.execute("DELETE FROM leases WHERE item = ?1", [item])?;
        tx.commit()?;
        Ok(outcome)
    }

    /// One item by id.
    pub fn show(&self, id: i64) -> Result<Option<Item>> {
        self.conn
            .query_row(
                "SELECT i.id, i.queue, i.key, i.payload, i.created_at, a.owner
                 FROM items i LEFT JOIN acks a ON a.item = i.id WHERE i.id = ?1",
                [id],
                item_from,
            )
            .optional()?
            .transpose()
    }

    /// Items in a queue, oldest first; with `unacked`, only the open ones.
    pub fn list(&self, queue: &str, unacked: bool, limit: i64) -> Result<Vec<Item>> {
        let mut st = self.conn.prepare(
            "SELECT i.id, i.queue, i.key, i.payload, i.created_at, a.owner
             FROM items i LEFT JOIN acks a ON a.item = i.id
             WHERE i.queue = ?1 AND (?2 = 0 OR a.item IS NULL)
             ORDER BY i.id DESC LIMIT ?3",
        )?;
        let mut items: Vec<Item> = st
            .query_map(params![queue, unacked, limit], item_from)?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .collect::<Result<_>>()?;
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

    /// Every subscriber's position.
    pub fn status(&self) -> Result<Vec<SubscriberState>> {
        let mut st = self.conn.prepare(
            "SELECT s.subscription, s.subscriber, p.queue, s.cursor, s.next_at, s.dropped,
               (SELECT count(*) FROM items i WHERE i.queue = p.queue AND i.id > s.cursor)
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

fn item_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Item>> {
    let payload: String = r.get(3)?;
    Ok(serde_json::from_str(&payload)
        .map_err(Error::from)
        .map(|payload| Item {
            id: r.get(0).unwrap_or_default(),
            queue: r.get(1).unwrap_or_default(),
            key: r.get(2).unwrap_or_default(),
            payload,
            created_at: r.get(4).unwrap_or_default(),
            acked_by: r.get(5).unwrap_or_default(),
        }))
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

fn scan(tx: &Connection, queue: &str, after: i64) -> Result<Vec<Row>> {
    let mut st = tx.prepare_cached(
        "SELECT id, queue, key, payload, created_at FROM items
         WHERE queue = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
    )?;
    let rows = st.query_map(params![queue, after, SCAN], |r| {
        Ok(Row {
            id: r.get(0)?,
            queue: r.get(1)?,
            key: r.get(2)?,
            payload: r.get(3)?,
            created_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn is_acked(tx: &Connection, id: i64) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM acks WHERE item = ?1)",
        [id],
        |r| r.get(0),
    )?)
}

fn lease_of(tx: &Connection, id: i64) -> Result<Option<(String, i64, i64)>> {
    Ok(tx
        .query_row(
            "SELECT owner, expires_at, attempts FROM leases WHERE item = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?)
}

/// The share: one low-water mark moves past leading rows that are done:
/// filtered out, acked, or parked. A leased row holds it.
fn advance_lwm(
    tx: &Connection,
    rules: &Rules,
    filter: Option<&Expr>,
    from: i64,
    rows: &[Row],
    now: i64,
) -> Result<i64> {
    let mut lwm = from;
    for row in rows {
        let skip = match filter {
            Some(f) => !f.matches(&row.value()?)?,
            None => false,
        };
        let parked = matches!(lease_of(tx, row.id)?,
                Some((_, until, a)) if a >= rules.max_attempts && until <= now)
            && !is_acked(tx, row.id)?
            && rules.consume == Consume::OnAck;
        if skip || is_acked(tx, row.id)? || parked {
            lwm = row.id;
        } else {
            break;
        }
    }
    Ok(lwm)
}

/// Open items this subscriber was shown in earlier pulls.
fn count_open(
    tx: &Connection,
    rules: &Rules,
    filter: Option<&Expr>,
    origin: i64,
    cursor: i64,
    just_shown: &[i64],
) -> Result<i64> {
    let mut st = tx.prepare_cached(
        "SELECT id, queue, key, payload, created_at FROM items i
         WHERE queue = ?1 AND id > ?2 AND id <= ?3
           AND NOT EXISTS (SELECT 1 FROM acks a WHERE a.item = i.id)
         ORDER BY id DESC LIMIT ?4",
    )?;
    let rows = st.query_map(params![rules.queue, origin, cursor, SCAN], |r| {
        Ok(Row {
            id: r.get(0)?,
            queue: r.get(1)?,
            key: r.get(2)?,
            payload: r.get(3)?,
            created_at: r.get(4)?,
        })
    })?;
    let mut n = 0;
    for row in rows {
        let row = row?;
        if just_shown.contains(&row.id) {
            continue;
        }
        if let Some(f) = filter
            && !f.matches(&row.value()?)?
        {
            continue;
        }
        n += 1;
    }
    Ok(n)
}

/// Cut a rendered item to `room` bytes, ending with a pointer to the
/// whole item.
fn cut(line: &str, room: usize, id: i64) -> String {
    let tail = format!(" … tundish show {id}\n");
    let keep = room.saturating_sub(tail.len());
    let mut end = keep.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{tail}", &line[..end])
}

/// Drop old items. `max_items` never trims past the lowest unread
/// position of any subscriber; `max_age` trims regardless and counts
/// the unread drops per subscriber.
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
                 WHERE p.queue = ?1
               UNION ALL
               SELECT lwm FROM subscriptions WHERE queue = ?1 AND json_extract(rules, '$.share') = 'one')",
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
    tx.execute(
        "UPDATE subscribers SET dropped = dropped + (
           SELECT count(*) FROM items i WHERE i.queue = ?1 AND i.id > subscribers.cursor AND i.id <= ?2)
         WHERE subscription IN (SELECT name FROM subscriptions WHERE queue = ?1)
           AND cursor < ?2",
        params![queue, cutoff],
    )?;
    tx.execute(
        "UPDATE subscribers SET cursor = ?2
         WHERE subscription IN (SELECT name FROM subscriptions WHERE queue = ?1) AND cursor < ?2",
        params![queue, cutoff],
    )?;
    tx.execute(
        "DELETE FROM acks WHERE item IN (SELECT id FROM items WHERE queue = ?1 AND id <= ?2)",
        params![queue, cutoff],
    )?;
    tx.execute(
        "DELETE FROM leases WHERE item IN (SELECT id FROM items WHERE queue = ?1 AND id <= ?2)",
        params![queue, cutoff],
    )?;
    tx.execute(
        "DELETE FROM items WHERE queue = ?1 AND id <= ?2",
        params![queue, cutoff],
    )?;
    Ok(())
}
