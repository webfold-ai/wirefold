//! Hostile bytes into the MessagePack decoder (deep nesting and huge declared lengths included).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wirefold::codec::msgpack::decode(data);
});
