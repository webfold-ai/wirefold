//! Hostile bytes into the framed decompressor: an error is fine, a panic, hang or unbounded
//! allocation is not.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wirefold::codec::try_decompress_framed(data);
});
