<!-- metadata
title: "What is tundish?"
description: "Queues, subscriptions, local storage, delivery, and retention"
type: explanation
-->

# What is tundish?

tundish is a local inbox for agents. A program publishes JSON items into
a queue. A subscription selects items from that queue and renders them
as text. A hook pulls the text into an agent's session.

The publisher and the agent do not need to run at the same time. Items
stay in the queue until retention removes them. Reading advances the
subscriber's delivery state. An agent can receive a CI result or a Slack
message at its next hook event without keeping a connection to the publisher.

## Queues and subscriptions

A queue groups items from a source or for a purpose. An item has an id,
a JSON payload, and a creation time. A publisher can add a key, such as
`slack:C123`, to label items from one channel. The key is a label for
filtering. Repeated keys still produce separate items.

A subscription defines a view of one queue. Its filter decides which
items match. Its template decides what text the agent receives. Two
subscriptions can read the same queue with different filters or delivery
rules. The publisher does not need to know about either subscription.

A subscriber is a stable name, often an agent role such as `reviewer`.
It binds to a subscription. Its position records progress across pulls and
across sessions. With the default rules, each subscriber has its own
position. With `share one`, subscribers use a shared position so that
successive pulls can divide the items among them.

A new subscriber starts with every item still kept. The `start new` rule
starts it after the existing items. The `latest` selection rule serves
another purpose: it delivers a recent selection and skips older unseen
items. It suits a status digest where an old item is no longer useful.

## Delivery and responsibility

`share` determines who receives an item. `consume` determines whether
delivery is enough or the item needs an ack.

The default, `all` with `on-read`, suits information for several agent
roles. Each role receives its own copy through its subscription binding.
Reading it does not require a further command.

With `all` and `on-ack`, several subscribers can see the same work. An
ack records who took responsibility. A second ack by another subscriber
exits 3 and names the owner. That subscriber can leave the item instead
of acting on it too. A repeat ack by the owner succeeds.

With `one` and `on-ack`, the pull itself grants a temporary lease. Every
subscriber, including the holder, skips the item while the lease holds.
If the holder stops without acking, the expired lease lets another pull
receive the item. After the maximum number of leases expires, tundish
parks the item. This limits repeated delivery of an item
that no subscriber finishes.

Each subscription owns its acks and leases. An ack in a `triage`
subscription does not take an item away from a separate `audit`
subscription on the same queue. The [delivery rules](cli-reference.md#delivery-rules)
list the combinations of `share` and `consume`.

## One local database

A hook already supplies the moment to run a pull. tundish does not need
a daemon to wait for that moment. A publish or pull opens the SQLite
database, does its work, and exits. SQLite holds the items, subscription
rules, positions, acks, and leases across those short processes.

SQLite uses WAL mode, and writes run in transactions. Concurrent hooks
therefore share one stored view of claims and positions. No network
listener or server sits between a publisher and the file. A publisher
that receives Slack events can hold its own network connection and call
the same Rust library through Python.

Each user has one database, set by `database:` in
`~/.config/tundish/config.yml`. It defaults to
`~/.local/state/tundish/tundish.db`. The publisher, handler, and agent
read the same config and reach the same database. The
[configuration](cli-reference.md#configuration) also controls retention.

## Pulls and hook events

A pull stops at the subscription's `--limit`, when set, or its byte
budget. `--max-bytes` defaults to 1024 bytes and has a minimum of 400;
there is no default item limit. Each subscription's output starts with a
status line, followed by its items. The first matching item is delivered
even if it must be cut to fit, ending with `… tundish show <id>`.
Output, including status lines, never exceeds the budget. The
[CLI reference](cli-reference.md#pull) gives the output rules and limits.

A hook can call `pull --due` after every tool batch. The subscription's
interval controls how often it delivers items. A pull that prints no
item does not spend that interval. The interval is not a timer that wakes
the agent: an idle session receives nothing until another hook event.

## At-least-once delivery

tundish prints and flushes the batch before committing the delivery.
If the process fails between those steps, the database still treats the
items as unread. A later pull can print them again. Committing first
would risk marking an item delivered before any output reached the hook.

gaff gives the handler a deadline through `GAFF_TIMEOUT_MS`. If that
deadline passes before commit, tundish rolls back and exits 1. gaff
discards the failed handler's output. A later pull can retry delivery.

The transaction covers delivery state inside tundish. It does not cover
an agent's actions in another program. An ack records responsibility;
it cannot prove that an external action finished or prevent that action
from happening twice.

## Retention

Retention limits the queue's stored history. The count limit waits for
unread items, so a slow subscriber can keep the queue above the configured
count. Once read positions permit it, a later publish removes the excess.
An unacked item already shown under `share all` has passed the read
position and can be removed by the count limit.

An age limit makes a different choice. It removes old items even when
a subscriber has not read them. tundish records the number of unread
matching items lost. A named pull or a due pull reports that count once
as `dropped=` in the status line, even without delivering an item from
the subscription. That count distinguishes an empty queue from missing
history.

Retention runs on publish. With no daemon, an idle queue does not remove
items at the instant they reach the age limit. A queue absent from the
config uses a count limit of 10000 and no age limit. The
[configuration reference](cli-reference.md#configuration) defines the
per-queue settings.
