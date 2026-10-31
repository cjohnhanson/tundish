# Contributing

## Use of AI

Say in the pull request when AI generated part of the change. Be able to
explain the change in your own words, and write your own comments on the
pull request. A pull request the author cannot
explain is closed.

## Setup

Install Rust through rustup and a C compiler for the bundled SQLite
library. `rust-toolchain.toml` selects the Rust toolchain. The
`--all-features` build includes the Python module and needs a Python
interpreter and its development library. CI uses Python 3.12.

Install `jq` and `uv` with your package manager. Install gaff for the
merge-gate tests and missouri for the suite under `tests/missouri/`:

```sh
cargo install --locked --git https://github.com/cjohnhanson/missouri
cargo install --locked --git https://github.com/cjohnhanson/gaff
```

Then build and test tundish:

```sh
git clone https://github.com/cjohnhanson/tundish
cd tundish
cargo build
cargo test --workspace --all-features
```

The missouri setup builds tundish and copies the binary into the suite's
command directory. Its wrapper sets database and config paths inside
each state directory.

## Before a large change

Open a GitHub issue first. A small fix needs none.

## What CI checks

For an outside contribution, open the pull request and stop there. The
maintainer records the required sign-off note on its head commit.

Three workflows check a change, and `release.yml` publishes a tag.
`verify` runs on every push. `gate` runs on pull requests and invokes
the local gates through `gaff ci`. `review-gate` checks the pull request's
sign-off note under the base branch's policy. It reads no code from the
pull request, so the branch cannot weaken that policy for itself.

The local commit gate checks formatting and clippy. It first refuses
unstaged Rust changes, so the checked files match the index. The merge
gate runs the Rust tests, Python tests, and missouri suite, then checks
the sign-off note. `verify` runs the same formatting, clippy, and test
commands:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
uv run --isolated --with pytest pytest -q tests/python
cd tests/missouri && missouri run
```

The missouri suite covers publishing through argv and stdin, pulling,
acks and competing acks, inspection commands, and delivery intervals.
The merge gate requires at least one passing path and no failures.
CI sets `MISSOURI_SANDBOX=preinstalled` for this suite.

The merge gate checks that every required sign-off has vendored criteria
and that every vendored criterion is required. `.gaff/gaff.yml`
names `review-tests`, `review-docs`, `review-code`, `review-deps`, and
`review-usability`. `gaff reviews` prints the list.

Each sign-off line names the criteria, verdict, commit, and evidence:

```
signoff[review-tests] PASS <sha> <evidence>
```

Put every line in one git note on that commit:

```sh
git notes --ref=reviews add -m '<the lines>' <sha>
```

`.agents/skills/signoff-driver/SKILL.md` has the procedure.

The checks read the note on the branch head, not GitHub's synthetic merge
commit. `gaff reviews check` refuses a missing sign-off, a `FAIL` verdict,
a line naming another commit, or two lines for one criterion. A sha must
have at least seven characters. Evidence must have at least three words.
Prose around the sign-off lines is ignored.

## Running the gates locally

`.gaff/gaff.yml` declares the gates. `gaff init --github` generates the
`verify` and `gate` workflows; `gaff init --git` installs the local git
hooks. To run the gates against `HEAD` without making a commit:

```sh
gaff ci
```

The pre-push hook refuses a push without the required sign-off note. For a
one-off contribution, leave the hooks uninstalled and let CI run on the
pull request. The local test commands read the working tree; the sign-off
note names the pushed commit.

## Pull requests

Branch from `main` and open the pull request from a fork. Keep a change
and its tests in one pull request. Write the commit message in the
imperative present. State what the change does and, where the diff does
not show it, why.

## Releases

Before a tag, run `sh scripts/prepublish.sh` from the repository root.
It needs `cargo-hack`, `cargo-audit`, `python3`, `curl`, and `rustup`
with the toolchain pinned in `rust-toolchain.toml`. The test prerequisites
above apply here too. It checks each feature and the feature powerset,
runs tests, clippy, and fmt, and audits the lockfile. It also runs a
publish dry run from an archive of `HEAD` and checks that the version
is absent from crates.io.

The release workflow in `.github/workflows/release.yml` uses trusted publishing (OIDC) for crates.io, PyPI, and npm, so these registries need no repository tokens.
Its only required repository secret is `HOMEBREW_TAP_TOKEN`, to update the tap.

Bump the version in `Cargo.toml`, commit it on `main`, tag `v<version>`,
and push the tag. The workflow checks that the tag is on `main` and
matches the manifest. It runs the Rust checks, the missouri suite, and
`cargo package --locked` before building the archives, wheels, source
distribution, npm packages, and Debian packages.

Registry publishing runs in order: crates.io, PyPI, then npm. The GitHub
release stays a draft until its required jobs pass. The Homebrew formula
updates after the release becomes public. A `workflow_dispatch` run
rehearses the builds without publishing a release or registry package.

## What not to commit

`.gitignore` covers `target/`, `dist/`, generated npm platform packages,
editor and agent state, and `.cargo/config.toml`. Do not commit local
databases or service tokens. An absolute home-directory path exposes an
account name and breaks other clones. Keep local dependency overrides
in `.cargo/config.toml`, not `Cargo.toml`.

## Questions

Open a GitHub issue.

## Security

Do not open a public issue for a vulnerability. See
[SECURITY.md](SECURITY.md).

## License

Contributions are licensed under MIT, the same as the project.
