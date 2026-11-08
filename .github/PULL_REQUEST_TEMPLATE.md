## Summary

<!-- What changes, and why. -->

## Type of change

- [ ] Bug fix
- [ ] New feature or API
- [ ] Documentation only
- [ ] Build, CI, or supply chain

## Checks

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --all-features --all-targets -- -D warnings`
- [ ] `cargo test --locked --all-features`
- [ ] `cargo check --locked --no-default-features`
- [ ] `cargo deny check advisories bans licenses sources` (if dependencies changed)
- [ ] A decoder or parser change comes with a hostile-input test (see `tests/codec_hostile_input.rs`, `tests/parser_dos.rs`)
- [ ] No `unsafe` code added outside `codec/web.rs`

## Notes for reviewers

<!-- Mention any new dependency and why it is needed. Say which feature combinations you built and tested. -->
