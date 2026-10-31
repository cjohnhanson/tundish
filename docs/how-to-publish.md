<!-- metadata
title: "How to publish into tundish from a program"
description: "Publish JSON from Python and receive Slack message events through Socket Mode"
type: guide
-->

# How to publish into tundish from a program

A publisher calls `tundish.publish` from Python or runs `tundish publish`.
Run it on the same machine and as the same user as the agent's hook.
Both read `~/.config/tundish/config.yml` and use its database.

## Publish from Python

Use CPython 3.10 or later. Install the package in your Python environment:

```sh
pip install tundish
```

In a uv project, use `uv add tundish`.

```python
import tundish

item_id = tundish.publish("slack", {"text": "hi"}, key="slack:C123")
print(item_id)
```

`publish` returns the integer item id after committing the item. It calls
the Rust library in-process. Set `database:` in
`~/.config/tundish/config.yml` to choose the user's database. The default
is `~/.local/state/tundish/tundish.db`. If the config file is absent,
every default applies.

Choose a queue for a source or kind of item, such as `slack` or `ci`.
Use a key for a label within that queue, such as `slack:C123` for a
channel. A key does not replace an earlier item or remove duplicates.
Each publish adds an item with a new id.

Keep payload fields and their types consistent. A subscription can then
filter `.payload.text` or `.payload.channel` without decoding text. Send
an object as the payload, not a string containing encoded JSON.

## Receive Slack message events

Create a Slack app and enable Socket Mode. Generate an app-level token
with `connections:write`. Under Event Subscriptions, enable events.
Slack's [Bolt setup instructions](https://docs.slack.dev/tools/bolt-python/creating-an-app/)
show these settings.

For public channels, add the bot event `message.channels` and the bot
scope `channels:history`. Install the app in your workspace, then invite
its bot to the channel. The
[message.channels reference](https://docs.slack.dev/reference/events/message.channels/)
describes the event and its scope.

Set both tokens in the environment of the receiver process. Replace the
placeholder values with the tokens from your Slack app:

```sh
export SLACK_BOT_TOKEN='xoxb-your-bot-token'
export SLACK_APP_TOKEN='xapp-your-app-token'
pip install slack_bolt
```

Save this as `slack_receiver.py`:

```python
import os

from slack_bolt import App
from slack_bolt.adapter.socket_mode import SocketModeHandler
import tundish

app = App(token=os.environ["SLACK_BOT_TOKEN"])


@app.event("message")
def publish_message(event):
    tundish.publish("slack", event, key=f"slack:{event['channel']}")


if __name__ == "__main__":
    SocketModeHandler(app, os.environ["SLACK_APP_TOKEN"]).start()
```

The official `slack_bolt` package's `SocketModeHandler` holds the
connection to Slack. The listener publishes each message event it receives.
It keeps the event object as the payload. See Slack's
[Socket Mode documentation](https://docs.slack.dev/tools/bolt-python/concepts/socket-mode/)
for the adapter.

Run the receiver as a long-lived process:

```sh
python slack_receiver.py
```

Keep it running while you receive messages. For unattended use, run this
command under your process manager with the two token variables set.
The receiver needs network access to Slack. tundish itself opens only the
local database. Run the receiver separately from the hook; the hook runs
the short `tundish pull` command.

## Pull one channel

Replace `C123` with your channel id:

```sh
tundish subscribe channel --as reviewer --queue slack --start new \
  --filter '.key == "slack:C123"' \
  --template '"[\(.queue)#\(.id)] \(.payload.text // .payload.message.text // "message event")"'
```

Post a message in that channel, then pull it:

```sh
tundish pull channel --as reviewer
```

The default `on-read` rule marks the item delivered after printing.
To receive future items in the agent's session, add the
[gaff handler](how-to-gaff.md#add-the-handler). Its `pull --due` command
also pulls this subscription for `reviewer`.
