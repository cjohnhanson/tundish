//! Errors. Each variant maps to one exit code in the CLI.

/// A tundish error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("expression: {0}")]
    Expr(String),
    #[error("{0}")]
    Usage(String),
    #[error("no subscription named {0}")]
    NoSubscription(String),
    #[error("{subscriber} is not subscribed to {subscription}; run `tundish subscribe {subscription} --as {subscriber}`")]
    NotSubscribed {
        subscription: String,
        subscriber: String,
    },
    #[error("the deadline passed before the pull committed; nothing was marked delivered")]
    Deadline,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A tundish result.
pub type Result<T> = std::result::Result<T, Error>;
