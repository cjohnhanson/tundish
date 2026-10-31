<!-- metadata
title: "Getting started with tundish"
description: "Publish an item, pull and ack it, then receive another through a gaff handler"
type: tutorial
-->

# Getting started with tundish

Install tundish with one of the commands in the
[README](https://github.com/cjohnhanson/tundish#install). The examples use the default database
and the subscriber name `reviewer`.

## Publish an item

```sh
tundish publish slack --key slack:C123 '{"text":"Review PR 42"}'
```

tundish prints an integer id. In a new database it prints `1`. If your id
differs, use it in the commands below. A queue needs no setup.

## Subscribe

```sh
tundish subscribe inbox --as reviewer --queue slack --consume on-ack
```

```text
created inbox
bound reviewer to inbox
rules: {"queue":"slack","select":"since_last_read","consume":"on_ack","share":"all","lease":600,"max_attempts":3,"start":"all"}
```

The subscription is `inbox`. It reads the queue `slack` for the subscriber
`reviewer`. A new subscriber starts with all items still kept, including
the item just published. `--consume on-ack` keeps an item open until a
subscriber acks it.

## Pull the batch

```sh
tundish pull inbox --as reviewer
```

```text
tundish: subscription=inbox subscriber=reviewer delivered=1 more=0 unacked=0 parked=0 errors=0 dropped=0 lease=0
[slack#1] slack:C123 {"text":"Review PR 42"}
```

The status line names the subscription and subscriber and reports one
delivered item. The item line starts with `[slack#1]`: queue `slack`, id
`1`. The key and payload follow. Use that id and the names from the status
line in the ack command below.

## Ack the item

When you take responsibility for the item, ack it:

```sh
tundish ack 1 --as reviewer --subscription inbox
```

```text
acked 1
```

Read the full item and its ack:

```sh
tundish show 1
```

Pull again:

```sh
tundish pull inbox --as reviewer
```

With no other items, this prints nothing.

## Receive an item in an agent session

Install [gaff](https://github.com/cjohnhanson/gaff) and set up its hooks
for your agent host. gaff runs a handler at hook events and injects its
output into the session.

Follow [Add the handler](how-to-gaff.md#add-the-handler) to configure the
`tundish` command for `reviewer`. Then follow
[Allow the handler in the working directory](how-to-gaff.md#allow-the-handler-in-the-working-directory)
to grant trust and check the setup. Keep the `inbox` subscription from
this tutorial; it has no delivery interval.

Start the agent in that directory. In another terminal, publish an item:

```sh
tundish publish slack --key slack:C123 '{"text":"PR 43 is ready for review"}'
```

Ask the agent to run `pwd`. After the tool batch, gaff runs the handler
and injects tundish's output. In the new database used above, the batch is:

```text
tundish: subscription=inbox subscriber=reviewer delivered=1 more=0 unacked=0 parked=0 errors=0 dropped=0 lease=0
[slack#2] slack:C123 {"text":"PR 43 is ready for review"}
```

The item has arrived in the agent's session. The
[tundish skill](https://github.com/cjohnhanson/tundish/blob/main/skills/tundish/SKILL.md) describes how an agent reads
the batch and acks an item.
