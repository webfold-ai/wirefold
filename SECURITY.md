# Security policy

## Reporting a vulnerability

Report suspected vulnerabilities privately, through either:

- GitHub private vulnerability reporting: <https://github.com/webfold-ai/wirefold/security/advisories/new>
- Email: **security@webfold.ai**

Do not open a public issue or pull request for an unpatched vulnerability. Include the affected version or
commit, the impact, and steps to reproduce (a minimal input that triggers it is ideal). The maintainers
will acknowledge reports as soon as they can, and will coordinate a fix and disclosure with the reporter.

## Supported versions

wirefold is pre-1.0. Security fixes are made against the latest `main` branch.

## Security-relevant parts of the crate

wirefold's main job is reading bytes and text that may come from someone else, so the decoders and parsers
are the part to review most carefully.

- **Decoders** (`codec::try_decompress_framed`, `codec::msgpack::decode`, `codec::proto::decode`,
  `codec::syntax::decerealize_term` and `decerealize_http`, `codec::get_at`, the `codec::web` openers) and
  **text parsers** (`codec::syntax::parse_surface` for JSON, YAML, TOON, POML, CSV, BAML and RDF/XML). The
  expected failure mode on hostile input is an error. A panic, a hang, a stack overflow or an allocation
  sized by the input rather than by the bytes actually present is a vulnerability. `tests/codec_hostile_input.rs`
  and `tests/parser_dos.rs` pin the behaviour found so far, and `fuzz/` holds cargo-fuzz targets for the same
  entry points.
- **`unsafe` code** is confined to `codec/web.rs`, where bytes are viewed as values at a coordinate. A bounds
  or alignment mistake there is memory-unsafe on attacker-controlled input, so changes to that file get the
  closest review.
- **The derive crate** (`wirefold_derive`) runs at build time. It has no dependencies. A defect in it can
  generate unsound code in a consumer, so generated-code changes are reviewed as security-relevant.
- **The apeiron dependency** is pinned to a commit. Changing that pin is a supply-chain decision and
  should be reviewed as one.

## Dependency and release security

CI runs `cargo audit` (RustSec advisories), `cargo deny` (licences, bans and sources), and generates a
CycloneDX SBOM. CodeQL and GitHub dependency review run where GitHub Advanced Security is enabled for the
repository. Tagged releases are packaged from the tag, signed keyless with cosign, and carry SLSA build
provenance (`.github/workflows/release.yml`). Dependabot proposes cargo and GitHub Actions updates weekly.
