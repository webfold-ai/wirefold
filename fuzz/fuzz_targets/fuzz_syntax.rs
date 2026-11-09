//! Hostile text into every structured-text surface. The first byte picks the surface. Also checks
//! that the borrowing parse agrees with the owned one.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wirefold::codec::syntax::{parse_surface, parse_surface_ref, Surface};

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
    let owned = parse_surface(surface, text);
    // The borrowing parser must agree with the owned one: same success or failure, same term.
    let borrowed =
        parse_surface_ref(surface, text).map(wirefold::codec::syntax::SyntaxTermRef::into_owned);
    assert_eq!(owned, borrowed);
});
