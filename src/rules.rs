//! A subscription's rules: what a pull selects, how it renders, and what
//! a delivery means.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Which items a pull selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Select {
    /// Every unseen item, oldest first, up to `limit`.
    #[default]
    SinceLastRead,
    /// The newest `limit` unseen items; older unseen items are skipped.
    Latest,
}

/// When an item counts as done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Consume {
    /// Delivery is enough.
    #[default]
    OnRead,
    /// An item is open until someone acks it by id.
    OnAck,
}

/// Who sees an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Share {
    /// Every subscriber sees every item.
    #[default]
    All,
    /// Each item goes to one subscriber.
    One,
}

/// Where a new subscriber's cursor starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Start {
    /// Every item still retained.
    #[default]
    All,
    /// Only items published after the subscriber joins.
    New,
}

/// The stored rules of one subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    pub queue: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default)]
    pub select: Select,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default)]
    pub consume: Consume,
    #[serde(default)]
    pub share: Share,
    /// Seconds between deliveries for `pull --due`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Seconds a `share: one` + `on_ack` lease holds.
    #[serde(default = "default_lease")]
    pub lease: i64,
    /// Leases before an item is parked.
    #[serde(default = "default_attempts")]
    pub max_attempts: i64,
    #[serde(default)]
    pub start: Start,
}

fn default_lease() -> i64 {
    600
}

fn default_attempts() -> i64 {
    3
}

impl Rules {
    /// Rules with every default, on `queue`.
    #[must_use]
    pub fn new(queue: &str) -> Self {
        Self {
            queue: queue.to_owned(),
            filter: None,
            select: Select::default(),
            limit: None,
            consume: Consume::default(),
            share: Share::default(),
            every: None,
            template: None,
            lease: default_lease(),
            max_attempts: default_attempts(),
            start: Start::default(),
        }
    }

    /// Reject combinations that have no meaning, and compile every
    /// expression so a typo fails at subscribe time, not at a pull.
    pub fn check(&self) -> Result<()> {
        if self.queue.is_empty() {
            return Err(Error::Usage("a subscription needs a queue".into()));
        }
        if self.select == Select::Latest && self.limit.is_none() {
            return Err(Error::Usage("select: latest needs a limit".into()));
        }
        if self.select == Select::Latest && self.share == Share::One {
            return Err(Error::Usage(
                "select: latest skips items, and share: one must not skip work; use since_last_read".into(),
            ));
        }
        if self.limit == Some(0) {
            return Err(Error::Usage("limit must be at least 1".into()));
        }
        if self.lease <= 0 || self.max_attempts <= 0 {
            return Err(Error::Usage(
                "lease and max_attempts must be positive".into(),
            ));
        }
        if let Some(f) = &self.filter {
            crate::expr::Expr::compile(f)?;
        }
        if let Some(t) = &self.template {
            crate::expr::Expr::compile(t)?;
        }
        Ok(())
    }
}

/// Parse `90`, `30s`, `10m`, `2h`, or `1d` into seconds.
pub fn duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: i64 = num
        .parse()
        .map_err(|_| Error::Usage(format!("not a duration: {s}")))?;
    let mult = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(Error::Usage(format!("not a duration: {s}"))),
    };
    if n <= 0 {
        return Err(Error::Usage(format!("a duration must be positive: {s}")));
    }
    n.checked_mul(mult)
        .ok_or_else(|| Error::Usage(format!("a duration is too long: {s}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration("90").unwrap(), 90);
        assert_eq!(duration("10m").unwrap(), 600);
        assert_eq!(duration("2h").unwrap(), 7200);
        assert!(duration("0m").is_err());
        assert!(duration("5w").is_err());
        assert!(duration("m").is_err());
    }

    #[test]
    fn latest_needs_a_limit_and_shared_items() {
        let mut r = Rules::new("q");
        r.select = Select::Latest;
        assert!(r.check().is_err());
        r.limit = Some(3);
        assert!(r.check().is_ok());
        r.share = Share::One;
        assert!(r.check().is_err());
    }

    #[test]
    fn a_bad_filter_fails_at_check() {
        let mut r = Rules::new("q");
        r.filter = Some(".a ==".into());
        assert!(r.check().is_err());
    }
}
