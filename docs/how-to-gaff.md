<!-- metadata
title: "Deliver tundish items through a gaff handler"
description: "Inject due batches into an agent session with a gaff handler"
type: guide
-->

# Deliver tundish items through a gaff handler

Install [gaff](https://github.com/cjohnhanson/gaff) and configure its hooks
for your agent host. Install tundish on the same machine. The publisher
and the handler must use the same database.

## Set a subscription interval

Choose a stable subscriber name for the agent role. Set a five-minute
interval for the `inbox` subscription:

```sh
tundish subscribe inbox --as reviewer --queue slack --consume on-ack --every 5m
```

With `--queue`, this command replaces all rules for `inbox`. If you have
a filter, template, or other rules to keep, include their flags too.
To bind another subscriber without changing the rules, omit `--queue`:

```sh
tundish subscribe inbox --as maintainer
```

## Add the handler

Find the executable:

```sh
command -v tundish
```

In `~/.config/gaff/handlers.yml`, add this entry under `handlers`. Replace
the command path with the absolute path printed above.

```yaml
handlers:
  - name: tundish
    command: [/absolute/path/to/tundish, pull, --due, --as, reviewer]
    events: [tool_batch, stop]
    every: {tool_calls: 1}
    timeout_ms: 400
    max_bytes: 1024
```

To restrict the handler to a repository, add a `when` field to the entry:

```yaml
    when: {cwd_prefix: /path/to/repo}
```

If you use another database, put `--db` and its absolute path in the
command before `pull`. If you use another config file, add `--config`
and its absolute path there too.

The handler's `every` counts tool calls. The subscription's `--every`
counts time since the last pull that printed an item. With `--every 5m`,
`pull --due` prints this subscription's items and notices together at
most once every five minutes. Between deliveries, the handler stays
silent. An empty pull does not start a window. An idle session waits
for its next hook event, even after five minutes pass.

## Allow the handler in the working directory

From a human terminal in the exact directory where the agent will start:

```sh
gaff trust
gaff check --handlers
gaff doctor
```

gaff records trust for that directory. A subdirectory needs its own trust
record. The `cwd_prefix` condition does not grant trust.

## Check delivery

Publish an item from another terminal:

```sh
tundish publish slack --key slack:C123 '{"text":"Review PR 42"}'
```

Ask the agent to run a command, such as `pwd`. At the next tool batch,
gaff injects the due batch. If the subscription has delivered in the last
five minutes, wait for the window to pass and trigger another tool batch.

To inspect the subscription's cursor and next delivery time:

```sh
tundish status
```

For a manual pull that ignores the interval:

```sh
tundish pull inbox --as reviewer
```

A manual pull marks its output delivered. If it prints an item, it also
starts a new five-minute window. The handler will not print that item
again under this subscription's `share all` rules.

## Keep the batch within the handler's budget

The example uses tundish's default 1024-byte pull budget and the same
`max_bytes` in gaff. If you change one, change the other. tundish includes
the footer lines in its budget. See the
[pull reference](cli-reference.md#pull) for limits on very small budgets.

gaff passes `GAFF_TIMEOUT_MS` to the command. If the deadline passes
before the commit, tundish rolls back and exits 1. gaff discards that
output. The next successful pull can deliver the items.
