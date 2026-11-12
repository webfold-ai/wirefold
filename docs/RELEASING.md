# Releasing wirefold

A release is a signed GitHub release for a `vX.Y.Z` tag. Publishing to crates.io is deliberately not part of it yet
(see the last section).

## Versioning

wirefold follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html), with the 0.x rule: while the major version
is 0, a breaking change to the public API or the wire format bumps the minor version, and anything else bumps the
patch. From 1.0.0 a breaking change bumps the major version.

What counts as breaking here:

- any change to the public API that `cargo semver-checks` reports;
- any change to the bytes `Collapse` writes or the reader accepts (the section-table version byte, the column
  layout, `codec::cm::VERSION`): data written by an older version must still open, or be refused with
  `WireErr::UnsupportedVersion`, never misread;
- raising the minimum supported Rust version (currently 1.89), which is a minor bump while 0.x.

Commits use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `perf:`, `docs:`, `ci:`,
`chore:`; a `!` or a `BREAKING CHANGE:` footer marks a break), because the bump is computed from them.

## Cutting a release

1. Be on an up-to-date `main` with a clean tree and green CI.
2. Run the release gate locally. It is the caretaker `rust-library-release` profile:

   ```bash
   cargo fmt --all -- --check
   cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
   cargo test --locked --workspace --all-features
   RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
   cargo deny check
   cargo audit
   cargo semver-checks check-release --package wirefold --all-features   # needs a previous tag
   ```

3. Compute the bump from the commits since the last tag with caretaker's `caretakers-bump` (the crate lives in
   [Ptyktos/caretaker](https://github.com/Ptyktos/caretaker), `caretaker-bump`):

   ```bash
   caretakers-bump --package wirefold plan     # prints the next version and the changelog entry, changes nothing
   caretakers-bump --package wirefold apply    # rewrites Cargo.toml, Cargo.lock and CHANGELOG.md, commits, tags
   ```

   If `plan` reports `bump: none` there is nothing to release. `wirefold_derive` is versioned in lockstep: set its
   `version` and the `wirefold_derive` requirement in the root `Cargo.toml` to the same number.
4. Review the changelog entry (it is generated from commit subjects, so tidy it by hand), open a pull request with the
   release commit, and merge it.
5. Push the tag: `git push origin vX.Y.Z`. The tag triggers `.github/workflows/release.yml`, which checks that the tag
   equals the package version, packages the source, signs it with Sigstore (keyless), attaches SLSA build provenance
   and creates the GitHub release with `SHA256SUMS`.
6. Verify a download:

   ```bash
   cosign verify-blob --certificate wirefold-vX.Y.Z-source.tar.gz.cert \
     --signature wirefold-vX.Y.Z-source.tar.gz.sig \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com \
     --certificate-identity-regexp '^https://github.com/webfold-ai/wirefold/\.github/workflows/release\.yml@refs/tags/vX\.Y\.Z$' \
     wirefold-vX.Y.Z-source.tar.gz
   ```

## The first release (0.1.0)

There is no earlier tag, so `cargo semver-checks` and `caretakers-bump plan` have no baseline: `plan` reports no bump
and the version in `Cargo.toml` is already 0.1.0. Release it by tagging the commit that carries the `[0.1.0]`
changelog section (step 5). The `semver` CI job skips itself until a tag exists.

## crates.io

Not part of a release for now. `cargo package` cannot succeed yet because `wirefold` depends on `apeiron`, which has no
0.1.0 on crates.io (only a placeholder). To publish: release `apeiron` 0.1.0 first, replace the git dependency in
`Cargo.toml` with the registry version, publish `wirefold_derive`, then `wirefold`, and add `cargo publish` to the
release workflow behind a protected environment.
