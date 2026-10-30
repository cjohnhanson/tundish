//! tundish: a local inbox for agents. Sources publish JSON items into
//! named queues; a subscription pulls, filters, and renders them.

pub mod cli;
pub mod config;
pub mod error;
pub mod expr;
mod mangen;
pub mod rules;
pub mod store;

pub use error::{Error, Result};
pub use store::{AckOutcome, Item, PullOpts, Retention, Store, SubscriberState};
