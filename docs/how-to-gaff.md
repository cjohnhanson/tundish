<!-- metadata
title: "How to deliver tundish items through a gaff handler"
description: "Inject due batches into an agent session with a gaff handler"
type: guide
-->

# How to deliver tundish items through a gaff handler

Install [gaff](https://github.com/cjohnhanson/gaff) and configure its hooks
for your agent host. Install tundish on the same machine. Run the
publisher, handler, and agent as the same user. They read
`~/.config/tundish/config.yml` and use its database.

## Set a subscription interval

Choose a stable subscriber name for the agent role. Set a five-minute
interval for the `inbox` subscription:

```sh
tundish subscribe inbox --as reviewer --queue slack --consume on-ack --every 5m
```

If `inbox` already reads `slack` and `reviewer` is already bound, this prints:

```text
replaced the rules of inbox
rules: {"queue":"slack","select":"since_last_read","consume":"on_ack","share":"all","every":300,"lease":600,"max_attempts":3,"start":"all"}
```

With `--queue`, this command replaces all rules for `inbox`. If you have
a filter, template, or other rules to keep, include their flags too.
To bind another subscriber without changing the rules, omit `--queue`:

```sh
tundish subscribe inbox --as maintainer
```

For a new binding, this prints:

```text
bound maintainer to inbox
rules: {"queue":"slack","select":"since_last_read","consume":"on_ack","share":"all","every":300,"lease":600,"max_attempts":3,"start":"all"}
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

To set the database path, use `database:` in
`~/.config/tundish/config.yml`. Use an absolute path so the handler can
run from any working directory. Without that setting, the database is
`~/.local/state/tundish/tundish.db`.

The handler's `every` counts tool calls. For the subscription's delivery
interval and output rules, see [Due pulls](cli-reference.md#due-pulls).
An idle session waits for its next hook event.

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
gaff runs the handler. If no batch arrives, check the subscription's
position and next delivery time:

```sh
tundish status
```

For a manual pull that ignores the interval:

```sh
tundish pull inbox --as reviewer
```

A manual pull marks its output delivered. The handler will not print
that item again under this subscription's `share all` rules.

## Keep the batch within the handler's budget

The example uses tundish's default 1024-byte pull budget and the same
`max_bytes` in gaff. If you change one, change the other. See
[Due pulls](cli-reference.md#due-pulls) for budget allocation and truncation.

gaff passes `GAFF_TIMEOUT_MS` to the command. If the deadline passes
before the commit, tundish rolls back and exits 1. gaff discards that
output. The next successful pull can deliver the items.
