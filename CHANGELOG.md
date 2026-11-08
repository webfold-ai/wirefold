# Changelog

All notable changes to this crate are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial release of wirefold: the serialization core split out of `hashtrinity` and renamed. It contains
  `codec` (zero-copy wire format, universal `Term`, integer column codec, context-mixing coder and the
  structured-text surfaces), the `Collapse`, `Term`, `Validate` and `Error` derives, and the `error` and
  `validate` modules the derives target.
- cargo-fuzz targets for the decoders and parsers, a supply-chain workflow (audit, deny, SBOM) and a signed
  release workflow with SLSA provenance.

### Changed

- Minimum supported Rust version is 1.89 (the derive crate already required it).
- `clippy -D warnings` is clean for the whole crate; a short list of inherited style lints is allowed by
  name in `src/lib.rs`.
