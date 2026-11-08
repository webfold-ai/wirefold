//! Hostile text into every structured-text surface. The first byte picks the surface.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wirefold::codec::syntax::{parse_surface, Surface};

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let Ok(text) = std::str::from_utf8(rest) else {
        return;
    };
    let surface = match sel % 7 {
        0 => Surface::Json,
        1 => Surface::Yaml,
        2 => Surface::Toon,
        3 => Surface::Poml,
        4 => Surface::Csv,
        5 => Surface::Baml,
        _ => Surface::RdfXml,
    };
    let _ = parse_surface(surface, text);
});
