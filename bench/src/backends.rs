//! The general-purpose compressors the report compares, behind one type. Each compresses a byte slice to a
//! self-contained byte vector and back; none is a dependency of the wirefold library.
//!
//! - `lz4`: the C reference implementation (liblz4) through the `lz4` crate, in its block format with the
//!   original length prepended. `Lz4Fast` is the default fast mode, `Lz4Hc(n)` the high-compression mode.
//! - `snap`: Snappy's raw format, pure Rust.
//! - `flate2`: zlib (deflate) through `flate2`'s default `miniz_oxide` backend, pure Rust.
//! - `brotli`: Brotli through the pure-Rust `brotli` crate, 4 MiB-class window.
//! - `zstd`: the C reference implementation through the `zstd` crate.
//!
//! The pure-Rust ports are slower than their C originals (the zlib and Brotli rows in particular), which the
//! report says wherever it shows their times.

use std::io::{Read, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// No compressor: the bytes as they are.
    None,
    Lz4Fast,
    Lz4Hc(i32),
    Snappy,
    Deflate(u32),
    Brotli(u32),
    Zstd(i32),
}

impl Backend {
    /// The name used in tables.
    pub fn name(self) -> String {
        match self {
            Backend::None => "none".to_string(),
            Backend::Lz4Fast => "lz4 (fast)".to_string(),
            Backend::Lz4Hc(l) => format!("lz4 hc {l}"),
            Backend::Snappy => "snappy".to_string(),
            Backend::Deflate(l) => format!("deflate {l}"),
            Backend::Brotli(q) => format!("brotli {q}"),
            Backend::Zstd(l) => format!("zstd {l}"),
        }
    }

    /// The compressor used to judge candidate encodings of a column: this one when it is fast enough to try
    /// several times per column, zstd -3 otherwise (the slow settings would spend the encode time on the
    /// search rather than on the compression being measured).
    pub fn probe(self) -> Backend {
        match self {
            Backend::None | Backend::Lz4Fast | Backend::Snappy | Backend::Deflate(1) => self,
            Backend::Zstd(l) if l <= 3 => self,
            _ => Backend::Zstd(3),
        }
    }

    pub fn compress(self, d: &[u8]) -> Vec<u8> {
        match self {
            Backend::None => d.to_vec(),
            Backend::Lz4Fast => lz4::block::compress(d, None, true).unwrap(),
            Backend::Lz4Hc(l) => lz4::block::compress(
                d,
                Some(lz4::block::CompressionMode::HIGHCOMPRESSION(l)),
                true,
            )
            .unwrap(),
            Backend::Snappy => snap::raw::Encoder::new().compress_vec(d).unwrap(),
            Backend::Deflate(l) => {
                let mut e =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(l));
                e.write_all(d).unwrap();
                e.finish().unwrap()
            }
            Backend::Brotli(q) => {
                let mut out = Vec::new();
                {
                    let mut w = brotli::CompressorWriter::new(&mut out, 1 << 16, q, 22);
                    w.write_all(d).unwrap();
                }
                out
            }
            Backend::Zstd(l) => zstd::encode_all(d, l).unwrap(),
        }
    }

    pub fn decompress(self, z: &[u8]) -> Vec<u8> {
        match self {
            Backend::None => z.to_vec(),
            Backend::Lz4Fast | Backend::Lz4Hc(_) => lz4::block::decompress(z, None).unwrap(),
            Backend::Snappy => snap::raw::Decoder::new().decompress_vec(z).unwrap(),
            Backend::Deflate(_) => {
                let mut out = Vec::new();
                flate2::read::ZlibDecoder::new(z)
                    .read_to_end(&mut out)
                    .unwrap();
                out
            }
            Backend::Brotli(_) => {
                let mut out = Vec::new();
                brotli::Decompressor::new(z, 1 << 16)
                    .read_to_end(&mut out)
                    .unwrap();
                out
            }
            Backend::Zstd(_) => zstd::decode_all(z).unwrap(),
        }
    }
}
