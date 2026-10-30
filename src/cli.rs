//! The command line. `run` is the whole program, so the binary and the
//! Python entry point share it.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Args as ClapArgs, Parser, Subcommand};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::rules::{Consume, Rules, Select, Share, Start, duration};
use crate::store::{AckOutcome, Change, PullOpts, Store};

/// The one-line description. `--help` and `prime` both print it.
pub const ABOUT: &str = "A local inbox that agents subscribe to";

/// Exit when an ack meets another owner's ack or lease.
pub const EXIT_TAKEN: i32 = 3;

static DOCS: &[(&str, &str)] = diataxis::embedded_docs!();

#[derive(Parser)]
#[command(name = "tundish", version, about = ABOUT)]
pub struct Args {
    /// The database file, in place of `database:` in the config. A test
    /// seam: printed commands assume the configured database.
    #[arg(long, global = true, hide = true)]
    db: Option<PathBuf>,

    /// The config file, in place of ~/.config/tundish/config.yml. A
    /// test seam.
    #[arg(long, global = true, hide = true)]
    config: Option<PathBuf>,

    /// The clock, in Unix seconds. A test seam.
    #[arg(long, global = true, hide = true)]
    now: Option<i64>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Publish one JSON item to a queue; print its id
    Publish {
        queue: String,
        /// A label to filter on, such as `slack:C123`
        #[arg(long)]
        key: Option<String>,
        /// The item; read from stdin when omitted
        json: Option<String>,
    },
    /// Bind a subscriber to a subscription; with --queue, create or replace it
    Subscribe(SubscribeArgs),
    /// Unbind a subscriber
    Unsubscribe {
        name: String,
        #[arg(long = "as")]
        subscriber: String,
    },
    /// Print the next batch for a subscriber and mark it delivered
    Pull(PullArgs),
    /// Ack an item by id
    Ack {
        id: i64,
        #[arg(long = "as")]
        subscriber: String,
        /// The subscription to ack in; needed when the subscriber holds
        /// more than one on the item's queue
        #[arg(long)]
        subscription: Option<String>,
    },
    /// Print one item as JSON
    Show { id: i64 },
    /// List queues, or the items in one queue
    List {
        queue: Option<String>,
        /// Only items no subscription has acked, or with --subscription,
        /// items that subscription has not acked
        #[arg(long)]
        unacked: bool,
        #[arg(long, requires = "unacked")]
        subscription: Option<String>,
        /// The newest N items
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Print every subscription and subscriber position
    Status,
    /// Check the config and every subscription's expressions
    Check,
    /// Print what tundish is and how to use it, for an agent's context
    Prime,
    /// Read the bundled documentation
    Docs(diataxis::DocsArgs),
    /// Write the man pages into a directory (the package build uses this)
    #[command(hide = true)]
    GenMan { dir: PathBuf },
    /// Print a shell completion script (the package build uses this)
    #[command(hide = true)]
    GenCompletions { shell: clap_complete::Shell },
}

#[derive(ClapArgs)]
struct SubscribeArgs {
    name: String,
    /// The subscriber: a stable name, such as an agent's role
    #[arg(long = "as")]
    subscriber: String,
    /// Create or replace the subscription on this queue
    #[arg(long)]
    queue: Option<String>,
    /// A jaq expression per item; the item matches when any output is truthy
    #[arg(long, requires = "queue")]
    filter: Option<String>,
    #[arg(long, value_enum, requires = "queue")]
    select: Option<SelectArg>,
    #[arg(long, requires = "queue")]
    limit: Option<usize>,
    #[arg(long, value_enum, requires = "queue")]
    consume: Option<ConsumeArg>,
    #[arg(long, value_enum, requires = "queue")]
    share: Option<ShareArg>,
    /// Minimum time between deliveries for `pull --due`: 90s, 10m, 1h
    #[arg(long, requires = "queue")]
    every: Option<String>,
    /// A jaq expression per item that yields the printed text
    #[arg(long, requires = "queue")]
    template: Option<String>,
    /// How long a lease holds under --share one and --consume on-ack
    #[arg(long, requires = "queue")]
    lease: Option<String>,
    /// Leases before an item is parked
    #[arg(long, requires = "queue")]
    max_attempts: Option<i64>,
    #[arg(long, value_enum, requires = "queue")]
    start: Option<StartArg>,
}

#[derive(ClapArgs)]
struct PullArgs {
    /// The subscription; omit with --due
    #[arg(required_unless_present = "due", conflicts_with = "due")]
    name: Option<String>,
    /// Pull every subscription of this subscriber whose `every` window has passed
    #[arg(long)]
    due: bool,
    #[arg(long = "as")]
    subscriber: String,
    /// The byte budget for the printed batch
    #[arg(long, default_value_t = 1024)]
    max_bytes: usize,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum SelectArg {
    SinceLastRead,
    Latest,
}
#[derive(Clone, Copy, clap::ValueEnum)]
enum ConsumeArg {
    OnRead,
    OnAck,
}
#[derive(Clone, Copy, clap::ValueEnum)]
enum ShareArg {
    All,
    One,
}
#[derive(Clone, Copy, clap::ValueEnum)]
enum StartArg {
    All,
    New,
}

/// What `prime` prints. A pure function of the binary.
#[must_use]
pub fn prime() -> String {
    // The shape is the contract and the test checks it.
    [
        "# tundish",
        ABOUT,
        "Programs publish JSON items into named queues in one SQLite file. A subscription filters and renders items; a cursor records subscriber progress. A pull prints a batch, then commits delivery. Within a subscription, an ack exits 3 if another subscriber has acked the item or holds its lease.",
        "Commands:",
        "  tundish publish <queue> [--key <k>] [<json>]",
        "  tundish subscribe <name> --as <subscriber> [--queue <q>]",
        "  tundish pull <name> --as <subscriber>",
        "  tundish pull --due --as <subscriber>",
        "  tundish ack <id> --as <subscriber> [--subscription <s>]",
        "  tundish show <id>",
        "  tundish status",
        "More: tundish --help; tundish docs",
        "",
    ]
    .join("\n")
}

fn clock(now: Option<i64>) -> i64 {
    now.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    })
}

/// The handler deadline gaff sets, less a margin for exit and flush.
fn deadline() -> Option<Instant> {
    let ms: u64 = std::env::var("GAFF_TIMEOUT_MS").ok()?.parse().ok()?;
    // A quarter of a short deadline, at most 100 ms, covers exit and flush.
    let margin = (ms / 4).min(100);
    Some(Instant::now() + Duration::from_millis(ms - margin))
}

/// Run the program on `argv`; return the exit code.
pub fn run<I, T>(argv: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let args = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    match dispatch(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tundish: {e}");
            match e {
                Error::Usage(_) | Error::NoSubscription(_) | Error::NotSubscribed { .. } => 2,
                _ => 1,
            }
        }
    }
}

fn dispatch(args: Args) -> Result<i32> {
    let now = clock(args.now);
    match args.command {
        Command::Prime => {
            print!("{}", prime());
            return Ok(0);
        }
        Command::Docs(d) => {
            let set = diataxis::DocSet::from_embedded(DOCS)
                .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
            return match d.request().and_then(|r| set.render(r)) {
                Ok(text) => {
                    print!("{text}");
                    Ok(0)
                }
                Err(e) => {
                    eprintln!("{e}\n\navailable docs:");
                    print!("{}", set.listing());
                    Ok(1)
                }
            };
        }
        Command::GenMan { dir } => {
            use clap::CommandFactory as _;
            std::fs::create_dir_all(&dir)?;
            crate::mangen::write_man_pages(&Args::command(), &dir)?;
            return Ok(0);
        }
        Command::GenCompletions { shell } => {
            use clap::CommandFactory as _;
            clap_complete::generate(
                shell,
                &mut Args::command(),
                "tundish",
                &mut std::io::stdout(),
            );
            return Ok(0);
        }
        _ => {}
    }

    let config = Config::load(args.config.as_deref())?;
    let path = args.db.unwrap_or_else(|| config.database.clone());
    let deadline = deadline();
    let busy = deadline.map_or(Duration::from_secs(5), |d| {
        d.saturating_duration_since(Instant::now())
    });
    let mut store = Store::open_with(&path, busy)?;
    let mut stdout = std::io::stdout().lock();

    match args.command {
        Command::Publish { queue, key, json } => {
            let text = match json {
                Some(t) => t,
                None => {
                    let mut t = String::new();
                    std::io::stdin().read_to_string(&mut t)?;
                    t
                }
            };
            let payload: serde_json::Value = serde_json::from_str(&text)?;
            let id = store.publish(
                &queue,
                key.as_deref(),
                &payload,
                now,
                config.retention(&queue),
            )?;
            writeln!(stdout, "{id}")?;
        }
        Command::Subscribe(s) => {
            let rules = s.queue.as_deref().map(|q| rules_from(q, &s)).transpose()?;
            let done = store.subscribe(&s.name, &s.subscriber, rules, now)?;
            let name = &s.name;
            match &done.change {
                Change::None => {}
                Change::Created => writeln!(stdout, "created {name}")?,
                Change::Replaced => writeln!(stdout, "replaced the rules of {name}")?,
                Change::Moved { from } => writeln!(
                    stdout,
                    "moved {name} from {from}; its positions, acks, and leases were reset"
                )?,
            }
            if done.bound {
                writeln!(stdout, "bound {} to {name}", s.subscriber)?;
            }
            writeln!(stdout, "rules: {}", serde_json::to_string(&done.rules)?)?;
        }
        Command::Unsubscribe { name, subscriber } => {
            if !store.unsubscribe(&name, &subscriber)? {
                return Err(Error::NotSubscribed {
                    subscription: name,
                    subscriber,
                });
            }
            writeln!(
                stdout,
                "unbound {subscriber} from {name}; its read position was removed"
            )?;
        }
        Command::Pull(p) => {
            let opts = PullOpts {
                now,
                max_bytes: p.max_bytes,
                deadline,
            };
            match p.name {
                Some(name) => store.pull(&name, &p.subscriber, &opts, &mut stdout)?,
                None => store.pull_due(&p.subscriber, &opts, &mut stdout)?,
            };
        }
        Command::Ack {
            id,
            subscriber,
            subscription,
        } => match store.ack_in(id, &subscriber, subscription.as_deref(), now)? {
            AckOutcome::Acked => writeln!(stdout, "acked {id}")?,
            AckOutcome::AlreadyAcked { owner, at } => {
                writeln!(stdout, "already acked by {owner} at {at}")?;
                return Ok(EXIT_TAKEN);
            }
            AckOutcome::LeasedBy { owner, until } => {
                writeln!(stdout, "leased by {owner} until {until}")?;
                return Ok(EXIT_TAKEN);
            }
            AckOutcome::NotFound => return Err(Error::Usage(format!("no item {id}"))),
        },
        Command::Show { id } => match store.show(id)? {
            Some(item) => writeln!(stdout, "{}", serde_json::to_string_pretty(&item)?)?,
            None => return Err(Error::Usage(format!("no item {id}"))),
        },
        Command::List {
            queue,
            unacked,
            subscription,
            limit,
        } => match queue {
            None => {
                for (q, n) in store.queues()? {
                    writeln!(stdout, "{q}\t{n}")?;
                }
            }
            Some(q) => {
                for item in store.list(&q, unacked, subscription.as_deref(), limit)? {
                    writeln!(stdout, "{}", serde_json::to_string(&item)?)?;
                }
            }
        },
        Command::Status => {
            for s in store.status(now)? {
                // The error text is JSON-quoted so a newline or tab in it
                // cannot break the line.
                let last_error = s
                    .last_error
                    .as_deref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .unwrap_or_default();
                writeln!(
                    stdout,
                    "{}\t{}\tqueue={}\tcursor={}\tunread={}\tnext_at={}\tdropped={}\terrors={}\tparked={}\tlast_error={last_error}",
                    s.subscription,
                    s.subscriber,
                    s.queue,
                    s.cursor,
                    s.unread,
                    s.next_at,
                    s.dropped,
                    s.errors,
                    s.parked
                )?;
            }
        }
        Command::Check => {
            for (name, rules) in store.subscriptions()? {
                rules
                    .check()
                    .map_err(|e| Error::Usage(format!("{name}: {e}")))?;
            }
            writeln!(stdout, "ok")?;
        }
        Command::Prime
        | Command::Docs(_)
        | Command::GenMan { .. }
        | Command::GenCompletions { .. } => {
            unreachable!("handled before the store opens")
        }
    }
    Ok(0)
}

fn rules_from(queue: &str, s: &SubscribeArgs) -> Result<Rules> {
    let mut r = Rules::new(queue);
    r.filter.clone_from(&s.filter);
    r.template.clone_from(&s.template);
    r.limit = s.limit;
    if let Some(v) = s.select {
        r.select = match v {
            SelectArg::SinceLastRead => Select::SinceLastRead,
            SelectArg::Latest => Select::Latest,
        };
    }
    if let Some(v) = s.consume {
        r.consume = match v {
            ConsumeArg::OnRead => Consume::OnRead,
            ConsumeArg::OnAck => Consume::OnAck,
        };
    }
    if let Some(v) = s.share {
        r.share = match v {
            ShareArg::All => Share::All,
            ShareArg::One => Share::One,
        };
    }
    if let Some(v) = s.start {
        r.start = match v {
            StartArg::All => Start::All,
            StartArg::New => Start::New,
        };
    }
    r.every = s.every.as_deref().map(duration).transpose()?;
    if let Some(l) = s.lease.as_deref() {
        r.lease = duration(l)?;
    }
    if let Some(m) = s.max_attempts {
        r.max_attempts = m;
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prime_follows_the_contract() {
        let p = prime();
        assert!(p.len() <= 700, "{} bytes", p.len());
        let lines: Vec<&str> = p.lines().collect();
        assert_eq!(lines[0], "# tundish");
        assert_eq!(lines[1], ABOUT);
        assert_eq!(*lines.last().unwrap(), "More: tundish --help; tundish docs");
        assert!(p.ends_with('\n') && !p.ends_with("\n\n"));
        assert!(!p.contains('\t') && !p.contains("[gaff:"));
        let cmds: Vec<&str> = lines
            .iter()
            .skip_while(|l| **l != "Commands:")
            .skip(1)
            .take_while(|l| l.starts_with("  "))
            .copied()
            .collect();
        assert!(!cmds.is_empty() && cmds.len() <= 7);
        for c in cmds {
            let argv: Vec<&str> = c
                .split_whitespace()
                .map(|w| w.trim_matches(|ch| ch == '[' || ch == ']'))
                .map(|w| if w.starts_with('<') { "1" } else { w })
                .collect();
            assert!(
                Args::try_parse_from(&argv).is_ok(),
                "prime command does not parse: {c}"
            );
        }
    }
}
