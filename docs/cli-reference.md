<!-- metadata
title: "CLI reference"
description: "Commands, subscription rules, configuration, output, and exit codes"
type: reference
-->

# CLI reference

## Global options

```text
tundish [--db <path>] [--config <file>] <command>
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--db <path>` | Config `database:`, else `~/.local/state/tundish/tundish.db` | SQLite database path. |
| `--config <file>` | `~/.config/tundish/config.yml` | Config file path. A file named with `--config` must exist. Only the default path may be absent when `--config` is omitted; then every default applies. |
| `-h`, `--help` | | Print help. Each command also accepts these flags. |
| `-V`, `--version` | | Print the version. |

`--db` and `--config` also work after a command name. The home directory
comes from the passwd database. `HOME` and XDG environment variables do
not change the default paths. There is no environment override for the
config path.

A missing file named with `--config` exits 2 and prints
`tundish: <path>: no such config file`.

## Item

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | Integer | Item id, unique across all queues in the database. |
| `queue` | String | Queue name. |
| `key` | String or null | Optional label set by the publisher, such as `slack:C123`. |
| `payload` | JSON | Published value. Object key order is kept. |
| `created_at` | Integer | Creation time in Unix seconds. |

Filters and templates receive this object. `show` and `list` also include
an `acks` array when at least one subscription has acked the item. Each
entry has `subscription`, `owner`, and `at` in Unix seconds.

## publish

```text
tundish publish <queue> [--key <k>] [<json>]
```

Publishes one item and prints its id. Without `<json>`, it reads JSON from
stdin. The queue name must be nonempty. The payload can be any JSON value.
`--key` sets an optional label. Reusing a key adds another item; it does
not replace or deduplicate items.

The insert and the queue's retention trim commit in one transaction.

## subscribe

```text
tundish subscribe <name> --as <subscriber> [--queue <q>] [<rule flags>]
```

With `--queue`, creates or replaces the named subscription and binds the
subscriber to it. Omitted rule flags take their defaults, including when
replacing a subscription. Without `--queue`, binds the subscriber to an
existing subscription. Rule flags require `--queue`.

Subscription and subscriber names must be nonempty. A subscriber is a
stable name, such as `reviewer`. Repeating the binding keeps its cursor.
Replacing rules on the same queue keeps existing positions and claims.
Moving the subscription to another queue resets its positions and removes
its acks and leases.

The command prints up to three lines, in this order:

| When | Output |
| --- | --- |
| `--queue` creates a subscription | `created <name>` |
| `--queue` replaces rules on the same queue | `replaced the rules of <name>` |
| `--queue` moves the subscription to another queue | `moved <name> from <old queue>; its positions, acks, and leases were reset` |
| This call binds the subscriber | `bound <subscriber> to <name>` |
| Always | `rules: <the stored rules as compact JSON>` |

Without `--queue`, no creation, replacement, or move line prints. An
existing binding prints no `bound` line. Run
`tundish subscribe <name> --as <subscriber>` with no other flags to read
the stored rules. If the subscriber is already bound, only `rules:` prints.

jaq evaluates jq expressions over JSON. Filters and templates see the
item object, including `.payload`. In a filter, `false` and `null` are
false; all other values are truthy.

| Rule flag | Default | Meaning |
| --- | --- | --- |
| `--filter <jaq>` | Match every item | Evaluate per item. Any truthy output makes it match. |
| `--select since-last-read\|latest` | `since-last-read` | Select unseen matching items. `latest` needs `--limit` and refuses `--share one`. |
| `--limit N` | No item limit | Maximum items per pull of this subscription. At least 1. |
| `--consume on-read\|on-ack` | `on-read` | Count delivery as done, or require an ack. |
| `--share all\|one` | `all` | Deliver to every subscriber, or share items among subscribers. |
| `--every <duration>` | No interval | Minimum interval for `pull --due`, per subscriber. |
| `--template <jaq>` | `[queue#id] key payload-json` | Render each item. The default omits the key when absent. |
| `--lease <duration>` | `600s` | Lease duration for `share one` with `on-ack`. |
| `--max-attempts N` | `3` | Maximum leases before parking an item. Must be positive. |
| `--start all\|new` | `all` | A new subscriber starts with all retained items, or only items published after it joins. |

Durations are positive integers with an optional unit: `90`, `30s`,
`10m`, `2h`, or `1d`. A bare integer means seconds.

`since-last-read` selects oldest first. `latest` selects the newest N
unseen matching items and prints them oldest first. It moves the cursor
to the newest scanned item and skips older unseen items. Each pull scans
at most 1000 items per subscription. For `latest`, this is the newest
1000 unseen items. If the byte budget stops a `latest` batch, the cursor
still moves to the newest scanned item; omitted items do not return.

Filter and template expressions compile when the subscription is created
or replaced. At pull time, a filter error skips the item and adds a
notice. A template joins multiple outputs with newlines. Strings print
as text; other values print as JSON. If a template fails, the default
item form prints with a template error notice. An empty result uses the
default form.

### Delivery rules

| Share | Consume | Result after a successful pull |
| --- | --- | --- |
| `all` | `on-read` | Each subscriber sees each selected item once. |
| `all` | `on-ack` | Each subscriber sees each selected item once. The first ack in the subscription wins. Pull notices count earlier unacked items. |
| `one` | `on-read` | Each selected item goes to exactly one subscriber. The pull records its ack. |
| `one` | `on-ack` | The pull leases each selected item to one subscriber. An expired lease makes it available again. |

With `share one` and `on-ack`, a live lease hides the item from every
subscriber, including its holder. After `--max-attempts` leases expire without an ack,
the item is parked. Pulls skip parked items. A parked notice follows the
[pull output rules](#pull).
The item remains available through `show` and `list` until retention
removes it.

Acks and leases belong to a subscription. Subscriptions on the same queue
do not share a claim.

## unsubscribe

```text
tundish unsubscribe <name> --as <subscriber>
```

Removes the subscriber's binding and cursor. The subscription remains,
even after its last subscriber leaves. Prints
`unbound <subscriber> from <name>; its read position was removed` and
exits 0. A later `subscribe` of the same subscriber starts a new position
under the subscription's `start` rule. An absent binding exits 2.

## pull

```text
tundish pull <name> --as <subscriber> [--max-bytes N]
tundish pull --due --as <subscriber> [--max-bytes N]
```

`--as` is required. A named pull reads one subscription and ignores its
`--every` interval. `--due` reads bound subscriptions whose windows have
passed, least recently delivered first, then by name. The name and
`--due` are mutually exclusive.

`--max-bytes` defaults to 1024 and must be at least 128. A due pull uses
one transaction and one byte budget across all subscriptions. It commits
all delivery changes or none. A subscriber with no bound subscriptions
gets exit 2 from `pull --due` and the error
`tundish: <who> holds no subscription, so nothing can be due: tundish subscribe <name> --as <who>`.
This catches a typo in `--as` or `--db` in a hook.
When the budget runs out, skipped
subscriptions print no line of their own and come first on the next due pull.

A pull prints and flushes the batch before committing delivery. A
failure before commit leaves delivery uncommitted. The next pull can
repeat printed items. Delivery is at least once.

A due pull omits a subscription's unacked and parked counts when it
delivers no item from that subscription. Filter-error and retention-drop
notices print once even without an item, since each reports a one-time
event. A bound subscriber with nothing due and nothing to report gets no
output and exit 0. A named pull prints all applicable notices even
without new items.
Retention drop counts remain pending until a pull reports them.

An `--every` window starts only after a pull that prints an item. This
also applies to a named pull. An empty pull or a named pull with only
notices does not start a window. See the [gaff guide](how-to-gaff.md) for
an interval used with a hook.

### Byte budget and footer

The pull reserves space for footer lines before rendering items. It
stops at the subscription's item limit, the byte budget, or the scan
limit. If the first item alone is too large, the pull cuts it and appends
`… tundish show <id>`. The cut item counts as delivered. Later items that
do not fit wait for another pull under `since-last-read`.

| Footer | Meaning |
| --- | --- |
| `ack: tundish ack <id> --as <subscriber> --subscription <name>` | The batch delivered items under `on-ack`. Replace `<id>` with an item id. With `share one`, the line also states the lease duration. |
| `N unacked in <name> from earlier pulls: tundish list <queue> --unacked --subscription <name>` | Earlier matching items remain unacked under `share all` with `on-ack`. |
| `N parked in <name> after M attempts: tundish list <queue> --unacked --subscription <name>` | The pull found items with no attempts left. |
| `N items in <name> failed the filter: <error>` | The filter failed on these items. The line gives the first error. For one item it says `1 item`. |
| `N unread items in <name> were dropped by retention` | Retention removed unread matching items for this subscriber. For one item it says `1 unread item in <name> was dropped by retention`. |
| `more in <name>: pull again` | The batch stopped before it finished scanning or delivering candidates. |

The budget includes footers. Footers are not truncated. A long name,
error message, or combination of notices can exceed the budget. If little
space remains for the first item, its
`show` pointer can also be cut. A larger `--max-bytes` leaves room for
both the item and its notices.

### Handler deadline

When `GAFF_TIMEOUT_MS` contains an unsigned integer, tundish uses it as
a deadline in milliseconds. It reserves one quarter of that time, up to
100 ms, for exit and flush. If the deadline passes before commit, the
pull rolls back and exits 1. gaff discards the failed handler's output.

## ack

```text
tundish ack <id> --as <subscriber> [--subscription <name>]
```

`--as` is required. `--subscription` is required only when the subscriber
holds more than one subscription on the item's queue. An explicit
subscription must be bound to that subscriber and read the item's queue.

| Result | Output | Exit |
| --- | --- | --- |
| First ack in the subscription | `acked <id>` | 0 |
| Repeat ack by the owner | `acked <id>` | 0 |
| Another subscriber already acked it | `already acked by <owner> at <unix time>` | 3 |
| Another subscriber holds a live lease | `leased by <owner> until <unix time>` | 3 |

An ack removes the lease. Exit 2 covers a missing item, an ambiguous
subscription, or a subscriber with no subscription on the item's queue.
It also covers an explicit `--subscription` that the subscriber does not
hold or that reads another queue.

## show

```text
tundish show <id>
```

Prints the full item as formatted JSON, with each subscription's ack.
A missing item exits 2.

## list

```text
tundish list [<queue>] [--unacked [--subscription <name>]] [--limit N]
```

Without a queue, prints queue names and item counts separated by a tab,
in queue name order. Only queues with retained items appear.

With a queue, prints one JSON item per line. `--limit` defaults to 50.
It selects the newest N items and prints them oldest first. A limit of
0 prints no items. `--limit=-1` removes the limit; `--limit -1` is a
usage error.

`--unacked` selects items with no ack in any subscription. With
`--subscription`, it selects items with no ack in that subscription.
`--subscription` requires `--unacked`. These options and `--limit` only
affect output when a queue is named. Listing does not change a cursor.

## status

```text
tundish status
```

Prints one tab-separated line per subscription and subscriber binding,
ordered by subscription then subscriber:

```text
<subscription> <subscriber> queue=<queue> cursor=<id> unread=<N> next_at=<unix time> dropped=<N>
```

`cursor` records progress. With `share one`, the cursor and unread count
refer to the subscription's shared position. `unread` counts items beyond
that position before filtering. For `share one`, it excludes acked items
but can include leased or parked items. `next_at` is the earliest time
for a due pull; 0 means there is no pending window. `dropped` counts
unread matching items removed by retention and not yet reported in a pull.

## check

```text
tundish check
```

Validates the config and every subscription's rules and expressions.
Prints `ok` on success.

## prime

```text
tundish prime
```

Prints a short description and command examples for an agent's context.
The output depends only on the binary version. It does not open the
database or load the config.

## docs

```text
tundish docs [topic]
tundish docs list
tundish docs search <query>
tundish docs --all
```

Without a topic, or with `list`, prints the bundled page listing. A topic
is a filename without `.md`, such as `getting-started`. `search` searches
the pages. `-a` or `--all` prints every page and cannot accompany a topic
or query. This command does not open the database or load the config.
An unknown topic, missing search query, or search with no matches exits 1.

## Package commands

`tundish help [command]` prints help for the command or the main command
listing.

These package commands are hidden from the main help:

| Command | Meaning |
| --- | --- |
| `tundish gen-man <dir>` | Write man pages into a directory. |
| `tundish gen-completions <shell>` | Print completions for `bash`, `elvish`, `fish`, `powershell`, or `zsh`. |

## Configuration

```yaml
format: 1
database: /path/to/tundish.db
queues:
  slack:
    max_items: 10000
    max_age: 7d
```

| Key | Default | Meaning |
| --- | --- | --- |
| `format` | Required in an existing file | Must be `1`. |
| `database` | `~/.local/state/tundish/tundish.db` | Database path. `--db` takes precedence. |
| `queues` | Empty map | Per-queue retention settings. |
| `queues.<queue>.max_items` | Unset for a named queue | Count limit, at least 1. Does not remove unread items. |
| `queues.<queue>.max_age` | Unset | Age limit as a duration. Removes old items even if unread. |

A queue absent from `queues` gets `max_items: 10000` and no age limit.
A named queue gets only the limits specified for it. An entry of `{}`
sets neither limit. Unknown config keys are errors. Paths in the file
are literal: `~` is not expanded, and a relative database path is relative
to the command's working directory.

Retention runs when publishing to the queue. `max_items` can keep more
than its limit while subscribers have unread items. It protects read
positions, not unacked work already shown under `share all`. `max_age`
removes items older than the configured duration regardless of position.
Unread matching items dropped for a subscriber contribute to its retention
notice. The [pull output rules](#pull) determine when that notice prints.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success, including a pull with no output when the subscriber is bound. |
| 1 | Error, including invalid JSON, expression errors, I/O or database errors, a missed deadline, or a failed docs request. |
| 2 | Usage error, a missing named config file, invalid config or rule values, unknown subscription, missing binding (including `pull --due` for a subscriber with no subscriptions), or missing item for `show` or `ack`. |
| 3 | An ack met another subscriber's ack or live lease. |

For a missing binding, the message names
`tundish subscribe <name> --as <subscriber>`. For an unknown subscription,
the message is `no subscription named <name>`.
