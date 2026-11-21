# tundish

A local inbox for coding agents, backed by SQLite.

tundish is one of the [eng.rip](https://eng.rip/) tools.

## Highlights

- [Accepts JSON items](https://tundish.eng.rip/guides/publish-items/) from programs into named queues in one SQLite file.
- [Filters items and formats text](https://tundish.eng.rip/guides/filter-and-format/) with jaq expressions for an agent's session.
- [Delivers CI results or Slack messages](https://tundish.eng.rip/guides/integration/) into an agent's session through a hook running `tundish pull`.
- [Supports publishing from Python](https://tundish.eng.rip/guides/integration/python/) through the `tundish` package.
- [Retains progress](https://tundish.eng.rip/concepts/queues-and-subscriptions/) across pulls and sessions.

## Installation

Supports macOS and Linux.

Install with one of:

```sh
brew install cjohnhanson/tap/tundish
cargo install --locked tundish
uv tool install tundish
npm install -g tundish
```

Prebuilt binaries are on the [GitHub releases page](https://github.com/cjohnhanson/tundish/releases); see the [installation guide](https://tundish.eng.rip/getting-started/installation/) for every method.

## Example

Publish a review request, then find the delivered item below the `tundish pull` summary.

```console
$ tundish publish slack --key slack:C123 '{"text":"Review PR 42"}'
1

$ tundish subscribe inbox --as reviewer --queue slack --consume on-ack
created inbox
bound reviewer to inbox
rules: {"queue":"slack","select":"since_last_read","consume":"on_ack","share":"all","lease":600,"max_attempts":3,"start":"all"}

$ tundish pull inbox --as reviewer
tundish: subscription=inbox subscriber=reviewer delivered=1 more=0 unacked=0 parked=0 errors=0 dropped=0 lease=0
[slack#1] slack:C123 {"text":"Review PR 42"}

$ tundish ack 1 --as reviewer --subscription inbox
acked 1

```

See the [integration guide](https://tundish.eng.rip/guides/integration/) to connect an agent.

## Documentation

Read the [documentation](https://tundish.eng.rip/getting-started/) online. Run `tundish docs` to list the bundled pages, or `tundish docs <page>` to print one.

For guides that combine tools, see [eng.rip guides](https://eng.rip/#guides).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE).
