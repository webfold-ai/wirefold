//! Hostile bytes into the universal-Term and HTTP-message decoders.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wirefold::codec::syntax::decerealize_term(data);
    let _ = wirefold::codec::syntax::decerealize_http(data);
});
