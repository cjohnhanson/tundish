# tundish

tundish is a local inbox for agents. Programs publish JSON items into named
queues in one SQLite file. A subscription filters the items and renders them
as text for a subscriber. A hook runs `tundish pull` and injects the printed
text into the agent's session.

## Install

The command is `tundish`, whichever package you install.

**[Archives of precompiled binaries for macOS and
Linux](https://github.com/cjohnhanson/tundish/releases)** cover x86-64 and
arm64. The Linux binaries are static executables. Each archive holds the
binary, its man pages, the README and the license. There is no Windows build.

| Package manager | Package | Command |
| --- | --- | --- |
| [Homebrew](https://brew.sh) | [cjohnhanson/tap/tundish](https://github.com/cjohnhanson/homebrew-tap) | `brew install cjohnhanson/tap/tundish` |
| [Cargo](https://doc.rust-lang.org/cargo/) | [tundish](https://crates.io/crates/tundish) | `cargo install tundish` |
| [uv](https://docs.astral.sh/uv/) | [tundish](https://pypi.org/project/tundish/) | `uv tool install tundish` |
| [pip](https://pip.pypa.io/) | [tundish](https://pypi.org/project/tundish/) | `pip install tundish` |
| [npm](https://www.npmjs.com) | [tundish](https://www.npmjs.com/package/tundish) | `npm install -g tundish` |
| [Nix](https://nixos.org/) | [flake](flake.nix) | `nix profile install github:cjohnhanson/tundish` |

### Building from source

tundish is written in Rust. It needs Rust 1.88 or newer and a C compiler
for the bundled SQLite library. The repository pins a toolchain in
`rust-toolchain.toml`.

```sh
git clone https://github.com/cjohnhanson/tundish
cd tundish
cargo build --release
./target/release/tundish --version
```

The binary is `target/release/tundish`. Put it on `PATH` to run the
commands below.

## Usage

```sh
tundish publish slack --key slack:C123 '{"text":"Review PR 42"}'
tundish subscribe inbox --as reviewer --queue slack --consume on-ack
tundish pull inbox --as reviewer
```

In a new database, the publish prints `1` and the subscribe prints:

```text
created inbox
bound reviewer to inbox
rules: {"queue":"slack","select":"since_last_read","consume":"on_ack","share":"all","lease":600,"max_attempts":3,"start":"all"}
```

The pull prints:

```text
[slack#1] slack:C123 {"text":"Review PR 42"}
ack: tundish ack <id> --as reviewer --subscription inbox
```

To take responsibility for the item, run the ack command with its id.
Keep `reviewer` as a stable subscriber name across sessions.

## How it works

A subscription stores a queue name, filter, template, and delivery rules.
A cursor records delivery progress. By default, every subscriber
receives each matching item once. With `share one` and
`on-ack`, a pull leases the item to one subscriber until an ack or lease
expiry. Acks and leases belong to that subscription. Publishing and
pulling open the local SQLite file and exit when done; tundish runs no
daemon.

## Python

The `tundish` Python package also installs the command. It supports
CPython 3.10 and later:

```python
import tundish

item_id = tundish.publish("slack", {"text": "hi"}, key="slack:C123")
```

## Documentation

- [Getting started](docs/getting-started.md): publish, pull, ack, and receive an item through a hook
- [Deliver tundish items through a gaff handler](docs/how-to-gaff.md): handler setup and delivery intervals
- [Publish into tundish from a program](docs/how-to-publish.md): Python and Slack Socket Mode
- [CLI reference](docs/cli-reference.md): commands, rules, config, and exit codes
- [What is tundish?](docs/what-is-tundish.md): queues, subscriptions, delivery, and retention

Run `tundish docs` to read the same documentation from the binary.

## License

MIT.
