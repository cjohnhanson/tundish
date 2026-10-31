<!-- metadata
title: "CLI reference"
description: "Commands, subscription rules, configuration, output, and exit codes"
type: reference
-->

# CLI reference

## Global options

```text
tundish <command>
```

| Option | Default | Meaning |
| --- | --- | --- |
| `-h`, `--help` | | Print help. Each command also accepts these flags. |
| `-V`, `--version` | | Print the version. |

tundish uses one database per user. The `database:` setting in
`~/.config/tundish/config.yml` selects its path. The default is
`~/.local/state/tundish/tundish.db`. If the config file is absent, every
default applies. Home comes from the passwd database. `HOME` and XDG
environment variables do not change these paths. The config path is fixed.

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

Subscription and subscriber names must contain 1 to 64 characters from
`A-Z a-z 0-9 . _ : -`. An invalid name exits 2. For example,
`tundish subscribe 'in box' --as reviewer --queue slack` prints:

```text
tundish: a subscription name is 1 to 64 of A-Z a-z 0-9 . _ : -, not "in box"
```

A subscriber is a stable name, such as `reviewer`. Repeating the binding
keeps its position. Replacing rules on the same queue keeps existing
positions and claims. A filter change does not revisit items behind the
position, even if they now match. This applies to both share modes.
Changing from `share one` to `share all` clears leases and carries the
position to the subscribers. Changing to `share one` starts from the
lowest subscriber position.
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
| `--every <duration>` | No interval | Interval for [due pulls](#due-pulls), per subscriber. |
| `--template <jaq>` | `[queue#id] key payload-json` | Render each item. The default omits the key when absent. |
| `--lease <duration>` | `600s` | Lease duration for `share one` with `on-ack`. |
| `--max-attempts N` | `3` | Maximum leases before parking an item. Must be positive. |
| `--start all\|new` | `all` | Start with all retained items, or only future items. Applies to each new subscriber under `share all` and the initial subscription position under `share one`. |

Durations are positive integers with an optional unit: `90`, `30s`,
`10m`, `2h`, or `1d`. A bare integer means seconds.

`since-last-read` selects oldest first. `latest` selects the newest N
unseen matching items and prints them oldest first. It moves the position
to the newest scanned item and skips older unseen items. Each pull scans
at most 1000 items per subscription. For `latest`, this is the newest
1000 unseen items. If the byte budget stops a `latest` batch, the pull
keeps the newest selected items that fit and skips older ones. It still
prints them oldest first and moves the position to the newest scanned
item. The status line reports `more=0` because the skipped items do not
return. See [Due pulls](#due-pulls) for a subscription with no room to
deliver an item.

Filter and template expressions compile when the subscription is created
or replaced. At pull time, a filter error skips the item and raises
`errors=` in the status line. `tundish status` keeps the most recent
filter error text in `last_error`.

A template joins multiple outputs with newlines. Strings print as text;
other values print as JSON. For `on-ack`, each item's output starts with
`[queue#id] ` before the template output. For `on-read`, the template
output prints as-is. If a template fails, the default item form prints
with ` (template error: <error>)` appended. Template errors do not raise
`errors=`. An empty result uses the default form.

### Delivery rules

| Share | Consume | Result after a successful pull |
| --- | --- | --- |
| `all` | `on-read` | Each subscriber sees each selected item once. |
| `all` | `on-ack` | Each subscriber sees each selected item once. The first ack in the subscription wins. `unacked=` counts earlier unacked items when the pull delivers items. |
| `one` | `on-read` | Each selected item goes to exactly one subscriber. The pull records its ack. |
| `one` | `on-ack` | The pull leases each selected item to one subscriber. An expired lease makes it available again. |

With `share one`, the subscription has one forward position for all its
subscribers. A pull reads new items after it. The position moves past
each item evaluated, whether delivered, filtered out, or skipped after a
filter error. A held lease does not block new work for other subscribers.

With `share one` and `on-ack`, a live lease hides the item from every
subscriber, including its holder. If it expires before an ack, the next
pull retries the item ahead of new items, within `--limit`. A retry does
not run the filter again. The item matched when first leased.

When the last allowed lease under `--max-attempts` expires without an
ack, the item is parked. A pull reports it once as `parked=` in the
[status line](#byte-budget-and-status-line). Parked items are never
retried. Raising `--max-attempts` does not bring them back. They remain
available through `show` and `list` until retention removes them.

Acks and leases belong to a subscription. Subscriptions on the same queue
do not share a claim.

## unsubscribe

```text
tundish unsubscribe <name> --as <subscriber>
```

Removes the subscriber's binding and position. The subscription remains,
even after its last subscriber leaves. Prints
`unbound <subscriber> from <name>; its read position was removed` and
exits 0. Under `share all`, a later `subscribe` starts a new position
under the subscription's `start` rule. Under `share one`, the subscription
keeps its position. An absent binding exits 2.

## pull

```text
tundish pull <name> --as <subscriber> [--max-bytes N]
tundish pull --due --as <subscriber> [--max-bytes N]
```

`--as` is required. A named pull reads one subscription and ignores its
`--every` interval. See [Due pulls](#due-pulls) for `--due`. The name and
`--due` are mutually exclusive.

`--max-bytes` defaults to 1024 and must be at least 400. A smaller value
exits 2 with `tundish: --max-bytes must be at least 400`.

A pull prints and flushes the batch before committing delivery. A
failure before commit leaves delivery uncommitted. The next pull can
repeat printed items. Delivery is at least once.

Earlier unacked items alone do not cause output. The `unacked=` count is
set only when the pull delivers items; otherwise it is 0. The `parked=`, `errors=`, and
`dropped=` counts print once, even without items. The pull that reports
them resets them. These rules apply to both named and due pulls.

### Due pulls

`pull --due --as <s>` visits the subscriber's due subscriptions in order
of `next_at`, then name. A subscription is due when `next_at` is no later
than the current time. After a delivery, `next_at` is the last delivery
time plus `--every`, using zero seconds when no interval is set. A named
pull also updates this time when it delivers an item. An empty pull or
a pull that prints only a status line does not move `next_at`.

A due pull uses one transaction and one byte budget across all
subscriptions. It commits all delivery changes or none. Each subscription
uses the budget left by those before it. A subscription with nothing to
report prints nothing and uses no budget.

If a subscription has no room for its status line plus the start of one
item, it prints nothing and stays due. It comes first on the next due
pull. If its status line fits but its first item does not, and other
output came before it, it prints `delivered=0 more=1` and keeps its
position. This also applies to `latest`.

The first item of the whole output is always delivered. If needed, it
is cut to fit and ends with `… tundish show <id>`. The cut item counts
as delivered. Output never exceeds the budget, including status lines
and newlines. These budget rules also apply to a named pull, which reads
one subscription.

A bound subscriber with nothing due and nothing to report gets no output
and exit 0. A subscriber with no bound subscriptions gets exit 2 and:

```text
tundish: <who> holds no subscription, so nothing can be due: tundish subscribe <name> --as <who>
```

See the [gaff guide](how-to-gaff.md) for an interval used with a hook.

### Byte budget and status line

Each subscription that prints anything starts with one status line,
followed by its item lines. The fields appear in this order:

```text
tundish: subscription=<name> subscriber=<subscriber> delivered=<N> more=<0|1> unacked=<N> parked=<N> errors=<N> dropped=<N> lease=<seconds>
```

| Field | Meaning |
| --- | --- |
| `subscription` | Subscription name. |
| `subscriber` | Subscriber name from `--as`. |
| `delivered` | Items delivered in this batch. |
| `more` | 1 when the batch stopped before finishing the scan or delivering all matching items. Pull again by the subscription name. A `latest` batch that delivered items skips older items and reports 0. |
| `unacked` | Earlier matching items delivered under `share all` with `on-ack` that nobody in the subscription acked. Set only alongside delivered items; otherwise 0. |
| `parked` | Items parked since the last report. |
| `errors` | Items whose filter failed since the last report. |
| `dropped` | Unread matching items removed by retention since the last report. Includes items trimmed under an open lease in `share one`. |
| `lease` | Seconds the lease holds for a `share one`, `on-ack` batch with delivered items; otherwise 0. |

Each subscription stops at its item limit, the byte budget, or the scan
limit. Later items that do not fit wait for another pull under
`since-last-read`. See [Due pulls](#due-pulls) for budget allocation and
truncation.

The ack command is documented by `tundish prime`
and the [tundish skill](../skills/tundish/SKILL.md). It takes the item id
from `[queue#id]` and the names from the status line. Inspect unacked items
with `tundish list <queue> --unacked --subscription <s>`. Inspect filter
error text with `tundish status`.

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
affect output when a queue is named. Listing does not change a position.

## status

```text
tundish status
```

Prints one tab-separated line per subscription and subscriber binding,
ordered by subscription then subscriber:

```text
<subscription>	<subscriber>	queue=<q>	cursor=<n>	unread=<n>	next_at=<t>	dropped=<n>	errors=<n>	parked=<n>	last_error=<json string or empty>
```

For example:

```text
inbox	reviewer	queue=slack	cursor=1	unread=0	next_at=1002	dropped=0	errors=0	parked=0	last_error=
```

`cursor=` shows the subscriber's position under `share all` and the
subscription's position under `share one`. With `share all`, `unread=`
counts items after the position before filtering. With `share one`, it
counts unacked items after the position plus expired leases waiting for
a retry. Live leases and parked items do not add to that count.

`next_at` is the earliest time for a [due pull](#due-pulls). It starts at
0. `dropped=` counts unread matching items removed by retention and not
yet reported in a pull. For `share one`, it includes items trimmed under
an open lease. `errors=` counts filter failures not yet reported in a
pull. `parked=` counts parked items, including those already reported.
`last_error` keeps the most recent filter error as a JSON string, or is
empty if none has occurred. Reporting errors in a pull resets their
count but never resets `last_error`.

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

The ack command in its output is:

```text
tundish ack <id> --as <subscriber> [--subscription <s>]
```

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

The config file is `~/.config/tundish/config.yml`. Home comes from the
passwd database. Publishers, the gaff handler, and the agent use the same
database because they read this file as the same user.

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
| `database` | `~/.local/state/tundish/tundish.db` | The user's database path. |
| `queues` | Empty map | Per-queue retention settings. |
| `queues.<queue>.max_items` | Unset for a named queue | Count limit, at least 1. Does not remove unread items. |
| `queues.<queue>.max_age` | Unset | Age limit as a duration. Removes old items even if unread. |

A queue absent from `queues` gets `max_items: 10000` and no age limit.
A named queue gets only the limits specified for it. An entry of `{}`
sets neither limit. Unknown config keys are errors. Paths in the file
are literal: `~` is not expanded, and a relative database path is relative
to the command's working directory.

Retention runs when publishing to the queue. `max_items` can keep more
than its limit while subscribers have unread items. Under `share all`,
it protects each subscriber's position. Unacked items already shown can
be removed.

Under `share one`, the retention floor comes from the subscription's
position and its lowest open lease. An open lease is neither acked nor
parked. The count limit keeps items after the position and from the
lowest open lease onward. Only subscriptions with a bound subscriber
hold retention. Parked items do not hold retention.

`max_age` removes items older than the configured duration regardless of
position or lease. Unread matching items removed by retention contribute
to `dropped=` in the status line. For `share one`, an item trimmed under
an open lease also counts. The count stays pending until a pull reports
it once.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success, including a pull with no output when the subscriber is bound. |
| 1 | Error, including invalid JSON, expression errors, I/O or database errors, a missed deadline, or a failed docs request. |
| 2 | Usage error, invalid config, names or rule values, unknown subscription, missing binding (including `pull --due` for a subscriber with no subscriptions), or missing item for `show` or `ack`. |
| 3 | An ack met another subscriber's ack or live lease. |

For a missing binding, the message names
`tundish subscribe <name> --as <subscriber>`. For an unknown subscription,
the message is `no subscription named <name>`.
