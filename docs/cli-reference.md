<!-- metadata
title: "CLI reference"
description: "Commands, subscription rules, configuration, output, and exit codes"
type: reference
-->

# CLI reference

Each command section is the output of `tundish <command> --help`. The
sections after them describe the item, the subscription rules, the pull
output, and the configuration file.

<!-- Begin generated CLI help: cargo test --test docs -->

## tundish

```text
A local inbox that agents subscribe to

Usage: tundish <COMMAND>

Commands:
  publish      Publish one JSON item to a queue; print its id
  subscribe    Bind a subscriber to a subscription; with --queue, create or replace it
  unsubscribe  Unbind a subscriber
  pull         Print the next batch for a subscriber and mark it delivered
  ack          Ack an item by id
  show         Print one item as JSON
  list         List queues, or the items in one queue
  status       Print every subscription and subscriber position
  check        Check the config and every subscription's expressions
  prime        Print what tundish is and how to use it, for an agent's context
  docs         Read the bundled documentation
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version

Exit codes:
  0  Success, including a pull with nothing to print
  1  An error: invalid JSON, an expression that fails to compile in
     subscribe or pull, a database or I/O error, a missed deadline, or a
     failed docs request
  2  A usage error: bad arguments, config, names, or rule values; a rule
     that fails `check`; an unknown subscription in subscribe, pull, or ack;
     a missing binding; a missing item
  3  An ack met another subscriber's ack or live lease
```

## tundish publish

```text
Publish one JSON item to a queue; print its id

Publishing also applies the queue's retention limits from the config, in the same transaction.
Reusing a key adds another item; it does not replace or deduplicate.

Usage: tundish publish [OPTIONS] <QUEUE> [JSON]

Arguments:
  <QUEUE>
          The queue; it needs no setup, and the name cannot be empty

  [JSON]
          The item, any JSON value; read from stdin when omitted

Options:
      --key <KEY>
          A label to filter on, such as `slack:C123`

  -h, --help
          Print help (see a summary with '-h')
```

## tundish subscribe

```text
Bind a subscriber to a subscription; with --queue, create or replace it

With --queue, creates the subscription or replaces all its rules; a rule flag left out takes its
default. Rule flags need --queue. Without --queue, binds the subscriber to an existing subscription
and changes no rule. For a subscriber already bound, that prints only the stored rules.

Prints up to three lines, in this order:

created <name>                  --queue made a new subscription replaced the rules of <name>
--queue named the same queue moved <name> from <queue>; ...  --queue named another queue bound
<subscriber> to <name>    this call made the binding rules: <the stored rules>       always, as JSON

Moving a subscription to another queue resets its positions and removes its acks and leases.

Usage: tundish subscribe [OPTIONS] --as <SUBSCRIBER> <NAME>

Arguments:
  <NAME>
          The subscription: 1 to 64 of A-Z a-z 0-9 . _ : -

Options:
      --as <SUBSCRIBER>
          The subscriber: a stable name, such as an agent's role

      --queue <QUEUE>
          Create or replace the subscription on this queue

      --filter <FILTER>
          A jaq expression per item; the item matches when any output is truthy [default: every item
          matches]

      --select <SELECT>
          Which unseen items a pull takes; `latest` needs --limit and refuses --share one [default:
          since-last-read]

          Possible values:
          - since-last-read: Every unseen item, oldest first
          - latest:          The newest unseen items up to --limit; older unseen items are skipped

      --limit <LIMIT>
          The most items one pull delivers, at least 1 [default: no limit]

      --consume <CONSUME>
          When an item counts as done [default: on-read]

          Possible values:
          - on-read: Delivery is enough
          - on-ack:  An item stays open until a subscriber acks it

      --share <SHARE>
          Who gets an item [default: all]

          Possible values:
          - all: Every subscriber gets every item
          - one: Each item goes to one subscriber

      --every <EVERY>
          Minimum time between deliveries for `pull --due`, per subscriber: 90, 30s, 10m, 2h, 1d
          [default: none]

      --template <TEMPLATE>
          A jaq expression per item that yields the printed text [default: `[queue#id] key payload`]

      --lease <LEASE>
          How long a lease holds under --share one and --consume on-ack [default: 600s]

      --max-attempts <MAX_ATTEMPTS>
          Leases before an item is parked, at least 1 [default: 3]

      --start <START>
          Where reading starts: for each new subscriber under --share all, and for the
          subscription's one shared position, set when --queue creates or moves it, under --share
          one [default: all]

          Possible values:
          - all: Every item still retained
          - new: Only items published after that point

  -h, --help
          Print help (see a summary with '-h')
```

## tundish unsubscribe

```text
Unbind a subscriber

Removes the binding and its read position, and prints `unbound <subscriber> from <name>; its read
position was removed`. The subscription stays, even with no subscriber left. An absent binding exits
2.

Usage: tundish unsubscribe --as <SUBSCRIBER> <NAME>

Arguments:
  <NAME>
          The subscription

Options:
      --as <SUBSCRIBER>
          The subscriber to unbind

  -h, --help
          Print help (see a summary with '-h')
```

## tundish pull

```text
Print the next batch for a subscriber and mark it delivered

Each subscription that prints anything starts with a status line, then one line per item. The batch
prints and flushes before the delivery commits, so a failure in between can repeat items on the next
pull.

When GAFF_TIMEOUT_MS holds a number of milliseconds, tundish keeps a quarter of it, at most 100 ms,
for exit and flush. If the rest passes before the commit, the pull rolls back and exits 1. The same
deadline bounds how long any command waits for a locked database.

Usage: tundish pull [OPTIONS] --as <SUBSCRIBER> [NAME]

Arguments:
  [NAME]
          The subscription; omit with --due

Options:
      --due
          Pull every subscription of this subscriber whose `every` window has passed

      --as <SUBSCRIBER>
          The subscriber pulling

      --max-bytes <MAX_BYTES>
          The byte budget for the printed batch, at least 400
          
          [default: 1024]

  -h, --help
          Print help (see a summary with '-h')
```

## tundish ack

```text
Ack an item by id

Prints `acked <id>` for the first ack in the subscription and for a repeat by the same subscriber,
and removes the lease. When another subscriber holds the item, exits 3 and prints `already acked by
<owner> at <unix time>` or `leased by <owner> until <unix time>`.

Exits 2 for a missing item, a subscriber with no subscription on the item's queue, more than one
such subscription without --subscription, or a --subscription the subscriber does not hold or that
reads another queue.

Usage: tundish ack [OPTIONS] --as <SUBSCRIBER> <ID>

Arguments:
  <ID>
          The item id, from `[queue#id]` in a pull

Options:
      --as <SUBSCRIBER>
          The subscriber that takes the item

      --subscription <SUBSCRIPTION>
          The subscription to ack in; needed when the subscriber holds more than one on the item's
          queue

  -h, --help
          Print help (see a summary with '-h')
```

## tundish show

```text
Print one item as JSON

Prints the item as indented JSON, with an `acks` array when any subscription has acked it. A missing
item exits 2.

Usage: tundish show <ID>

Arguments:
  <ID>
          The item id

Options:
  -h, --help
          Print help (see a summary with '-h')
```

## tundish list

```text
List queues, or the items in one queue

Without a queue, prints each queue that holds items and its item count, separated by a tab; the
other options then change nothing. With a queue, prints one JSON item per line, the newest N oldest
first. Listing moves no position.

Usage: tundish list [OPTIONS] [QUEUE]

Arguments:
  [QUEUE]
          The queue whose items to print

Options:
      --unacked
          Only items no subscription has acked, or with --subscription, items that subscription has
          not acked

      --subscription <SUBSCRIPTION>
          With --unacked, the subscription whose acks count. The name is not checked: an unknown
          name holds no ack, so every item prints

      --limit <LIMIT>
          The newest N items; 0 prints none, and `--limit=-1` prints every item (`--limit -1` is a
          usage error)
          
          [default: 50]

  -h, --help
          Print help (see a summary with '-h')
```

## tundish status

```text
Print every subscription and subscriber position

Prints one tab-separated line per binding, by subscription then subscriber: `<subscription>
<subscriber> queue= cursor= unread= next_at= dropped= errors= parked= last_error=`. `last_error` is
the most recent filter error as a JSON string, or empty.

Usage: tundish status

Options:
  -h, --help
          Print help (see a summary with '-h')
```

## tundish check

```text
Check the config and every subscription's expressions

Prints `ok`, or names the first subscription whose rules fail and exits 2.

Usage: tundish check

Options:
  -h, --help
          Print help (see a summary with '-h')
```

## tundish prime

```text
Print what tundish is and how to use it, for an agent's context

The text depends only on the binary; it opens no database and reads no config.

Usage: tundish prime

Options:
  -h, --help
          Print help (see a summary with '-h')
```

## tundish docs

```text
Read the bundled documentation

Without a topic, lists the pages. --all cannot accompany a topic. An unknown topic, a search with no
query, or a search with no match exits 1.

Usage: tundish docs [OPTIONS] [TOPIC] [QUERY]

Arguments:
  [TOPIC]
          Topic slug to show, or "search" to search the docs

  [QUERY]
          Search query (when topic is "search")

Options:
  -a, --all
          Print every page whole, in one stream

  -h, --help
          Print help (see a summary with '-h')
```

<!-- End generated CLI help -->

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

## Subscription rules

Subscription and subscriber names must contain 1 to 64 characters from
`A-Z a-z 0-9 . _ : -`. An invalid name exits 2.

A subscriber is a stable name, such as `reviewer`. Repeating the binding
keeps its position. Replacing rules on the same queue keeps existing
positions and claims. A filter change does not revisit items behind the
position, even if they now match. This applies to both share modes.
Changing from `share one` to `share all` clears leases and carries the
position to the subscribers. Changing to `share one` starts from the
lowest subscriber position.

jaq evaluates jq expressions over JSON. Filters and templates see the
item object, including `.payload`. In a filter, `false` and `null` are
false; all other values are truthy.

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

## Delivery rules

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

Under `share all`, a subscriber that unsubscribes and subscribes again
starts a new position under the subscription's `start` rule. Under
`share one`, the subscription keeps its position.

## Due pulls

`pull --due --as <s>` visits the subscriber's due subscriptions in order
of `next_at`, then name. A subscription is due when `next_at` is no later
than the current time. After a delivery, `next_at` is the last delivery
time plus `--every`, using zero seconds when no interval is set. A named
pull ignores `--every`, and also updates this time when it delivers an
item. An empty pull or a pull that prints only a status line does not
move `next_at`.

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

## Byte budget and status line

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

Earlier unacked items alone do not cause output. The `parked=`,
`errors=`, and `dropped=` counts print once, even without items. The pull
that reports them resets them.

Each subscription stops at its item limit, the byte budget, or the scan
limit. Later items that do not fit wait for another pull under
`since-last-read`. See [Due pulls](#due-pulls) for budget allocation and
truncation.

## Status fields

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
Reporting errors in a pull resets their count but never resets
`last_error`.

## Package commands

These commands are hidden from the main help:

| Command | Meaning |
| --- | --- |
| `tundish gen-man <dir>` | Write man pages into a directory. |
| `tundish gen-completions <shell>` | Print completions for `bash`, `elvish`, `fish`, `powershell`, or `zsh`. |

## Configuration

The config file is `~/.config/tundish/config.yml`. Home comes from the
passwd database; `HOME` and XDG variables do not change the path.
Publishers, the gaff handler, and the agent use the same database because
they read this file as the same user. If the file is absent, every default
applies.

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
