//! The user config: `~/.config/tundish/config.yml`, home from the
//! passwd database. The path takes no environment override; tests name
//! a file with `--config`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::rules::duration;
use crate::store::Retention;

/// The retention a queue gets when the config names none.
pub const DEFAULT_RETENTION: Retention = Retention {
    max_items: Some(10_000),
    max_age: None,
};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    format: u32,
    database: Option<PathBuf>,
    #[serde(default)]
    queues: BTreeMap<String, RawRetention>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRetention {
    max_items: Option<i64>,
    max_age: Option<String>,
}

/// The parsed config.
#[derive(Debug, Clone)]
pub struct Config {
    pub database: PathBuf,
    queues: BTreeMap<String, Retention>,
}

impl Config {
    /// The retention for one queue.
    #[must_use]
    pub fn retention(&self, queue: &str) -> Retention {
        self.queues.get(queue).copied().unwrap_or(DEFAULT_RETENTION)
    }

    /// Load from `path`, or from the fixed path when `None`. A missing
    /// file means every default.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let home = mdstore::userconfig::passwd_home()
            .ok_or_else(|| Error::Usage("no home directory in the passwd database".into()))?;
        let path = path.map_or_else(
            || home.join(".config/tundish/config.yml"),
            Path::to_path_buf,
        );
        let raw: Raw = match std::fs::read_to_string(&path) {
            Ok(text) => yaml_serde::from_str(&text)
                .map_err(|e| Error::Usage(format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Raw {
                format: 1,
                ..Raw::default()
            },
            Err(e) => return Err(e.into()),
        };
        if raw.format != 1 {
            return Err(Error::Usage(format!(
                "{}: format {} is not one this tundish reads (1)",
                path.display(),
                raw.format
            )));
        }
        let mut queues = BTreeMap::new();
        for (name, r) in raw.queues {
            let max_age = r.max_age.as_deref().map(duration).transpose()?;
            queues.insert(
                name,
                Retention {
                    max_items: r.max_items,
                    max_age,
                },
            );
        }
        Ok(Self {
            database: raw
                .database
                .unwrap_or_else(|| home.join(".local/state/tundish/tundish.db")),
            queues,
        })
    }
}
