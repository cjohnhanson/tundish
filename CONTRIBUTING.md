# Contributing

## Use of AI

TODO(copy)

## Setup

TODO(copy)

```sh
git clone https://github.com/cjohnhanson/tundish
cd tundish
cargo build
cargo test --workspace --all-features
```

TODO(copy)

```sh
cargo install --locked --git https://github.com/cjohnhanson/missouri
cargo install --locked --git https://github.com/cjohnhanson/gaff
```

TODO(copy)

## Before a large change

TODO(copy)

## What CI checks

TODO(copy)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cd tests/missouri && missouri run
```

TODO(copy)

```
signoff[review-tests] PASS <sha> <evidence>
```

TODO(copy)

```sh
git notes --ref=reviews add -m '<the lines>' <sha>
```

TODO(copy)

## Running the gates locally

TODO(copy)

```sh
cargo install --locked --git https://github.com/cjohnhanson/gaff
gaff ci
```

TODO(copy)

## Pull requests

TODO(copy)

## Releases

TODO(copy)

## What not to commit

TODO(copy)

## Questions

TODO(copy)

## Security

TODO(copy)

## License

TODO(copy)
