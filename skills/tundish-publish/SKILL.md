---
name: tundish-publish
description: Write a tundish publisher when a program needs to put JSON items into an agent's local inbox.
---

# Publishing to tundish

A publisher adds JSON items to a named queue. It can call the CLI or
the Python API. Publishing returns an integer id after commit.

## Choose the queue and key

Use a stable queue name for the source or kind of item, such as `slack`
or `ci`. A subscription reads one queue. Check existing queue names with
`tundish list` before adding a new one.

Use the optional key as a label within the queue. For Slack messages,
use `slack:<channel>`, such as `slack:C123`. The key supports filtering;
it does not deduplicate or replace items. Each publish adds a new id.
If the source can repeat an event, keep its event id in the payload so
the consumer can recognize the repeat.

## Shape the payload

Use a JSON object with stable field names and types. Keep fields used
by filters separate from display text. A CI item might be:

```json
{"repo":"example/api","run_id":42,"status":"failed","text":"Tests failed"}
```

The filter receives `{id, queue, key, payload, created_at}`. It reads
`.payload.status`, not `.status`. A subscription can select this item
with `--filter '.payload.status == "failed"'`. Preserve source fields
when publishing an existing event object. Do not encode that object
as a JSON string inside the payload.

## Publish with the CLI

```sh
tundish publish ci --key ci:example/api \
  '{"repo":"example/api","run_id":42,"status":"failed","text":"Tests failed"}'
```

Without a JSON argument, tundish reads stdin:

```sh
tundish publish ci --key ci:example/api < event.json
```

When calling from a program, serialize the payload and pass it through
stdin or an argv element. Do not interpolate event text into shell code.
Check the exit status before treating the item as published.

## Publish with Python

Install with `pip install tundish` or `uv add tundish`. CPython 3.10
and later can use the abi3 wheel.

```python
import tundish

item_id = tundish.publish(
    "ci",
    {"repo": "example/api", "run_id": 42, "status": "failed", "text": "Tests failed"},
    key="ci:example/api",
)
```

The call runs the Rust library in-process. Publishers, hooks, and agents
running as the same user read `~/.config/tundish/config.yml` and use its
`database:` setting. The database defaults to
`~/.local/state/tundish/tundish.db`. If the config file is absent, every
default applies.

For a receiver that waits on an external service, keep that receiver
running as its own process. The hook only needs to run `tundish pull`.
`tundish docs how-to-publish` has a Slack Socket Mode example using
`slack_bolt`.
