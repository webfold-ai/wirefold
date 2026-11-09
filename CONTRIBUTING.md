# Contributing

Thank you for considering a contribution. wirefold is a dependency of other Ptyktos and webfold crates,
so a change here can affect all of them. Keep pull requests focused and explain the reason for each change.

## Before you open a pull request

- Search existing issues and pull requests.
- Include or update a test when behaviour changes.
- A change to a decoder or parser comes with a hostile-input test in `tests/codec_hostile_input.rs` or
  `tests/parser_dos.rs`, and the fuzz target for that entry point still builds (`cd fuzz && cargo check`).
- If you change a feature-gated module, say which feature combinations you built and tested
  (`default`, `--no-default-features`, `--all-features`).

## Checks

Run these before you push. They are the same commands CI runs.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo test --locked --all-features
cargo check --locked --no-default-features
```

The minimum supported Rust version is 1.89. CI checks it.

Changes to `unsafe` code, the pool or the parallel parsers also run under the sanitizers (nightly, with
`rust-src`). AddressSanitizer's leak check is on for every test:

```sh
RUSTFLAGS="-Zsanitizer=address" cargo +nightly test --all-features --tests --target x86_64-unknown-linux-gnu
RUSTFLAGS="-Zsanitizer=thread" cargo +nightly test -Zbuild-std --all-features --tests --target x86_64-unknown-linux-gnu
cargo +nightly miri test --all-features --lib -- tans::tests::tans_roundtrip huf:: codec::web::tests
```

Miri is slow (the tANS and Huffman round trips take about 8 minutes), so it is run on the `unsafe` modules and not in CI.

## Benchmarks

Benchmarks live in `bench/`, outside the library workspace, so the library stays dependency-free. See
[`docs/BENCHMARKS.md`](docs/BENCHMARKS.md) for how to run them. A performance claim in a pull request needs
a benchmark result that can be reproduced from a clean checkout.

## Dependencies

wirefold depends on apeiron only. A new dependency needs a strong reason, because the library's value is
that it adds nothing to your build. Git dependencies are pinned in `Cargo.toml` and must not be pinned to a
branch.

## Pull requests

Open a pull request against `main`. Describe the change, the commands you ran and their results, and any
security or compatibility impact.

## Security reports

Do not report vulnerabilities in public issues or pull requests. Follow [SECURITY.md](SECURITY.md).

## Licence

By contributing, you agree that your contribution is offered under the Apache License, Version 2.0.
