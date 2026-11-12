//! The API both front ends wrap.

use std::fmt;

use wirefold::codec::msgpack;
use wirefold::codec::syntax::{
    parse_surface, render_surface, translate as translate_surfaces, Surface,
};

/// Why a call failed. The text is meant for a person: it names the input that was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError(String);

impl ApiError {
    fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

/// The surface names, in the order wirefold lists them.
pub const FORMATS: [&str; 8] = [
    "json", "yaml", "toon", "poml", "csv", "baml", "turtle", "rdfxml",
];

/// The crate version.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn surface(name: &str) -> Result<Surface, ApiError> {
    match name.trim().to_ascii_lowercase().as_str() {
        "json" => Ok(Surface::Json),
        "yaml" | "yml" => Ok(Surface::Yaml),
        "toon" => Ok(Surface::Toon),
        "poml" => Ok(Surface::Poml),
        "csv" => Ok(Surface::Csv),
        "baml" => Ok(Surface::Baml),
        "turtle" | "ttl" => Ok(Surface::Turtle),
        "rdfxml" | "rdf/xml" | "rdf+xml" => Ok(Surface::RdfXml),
        other => Err(ApiError::new(format!(
            "unknown format {other:?}; expected one of {}",
            FORMATS.join(", ")
        ))),
    }
}

/// Convert `input` from one surface to another.
///
/// # Errors
/// An unknown format name, or input that does not parse as `from`.
pub fn translate(input: &str, from: &str, to: &str) -> Result<String, ApiError> {
    translate_surfaces(input, surface(from)?, surface(to)?)
        .map_err(|e| ApiError::new(format!("cannot read the input as {from}: {}", e.msg)))
}

/// Convert JSON text to MessagePack.
///
/// # Errors
/// Input that is not valid JSON.
pub fn json_to_msgpack(json: &str) -> Result<Vec<u8>, ApiError> {
    let term = parse_surface(Surface::Json, json)
        .map_err(|e| ApiError::new(format!("cannot read the input as json: {}", e.msg)))?;
    Ok(msgpack::encode(&term))
}

/// Convert MessagePack bytes to JSON text.
///
/// # Errors
/// Bytes that are not well-formed MessagePack.
pub fn msgpack_to_json(bytes: &[u8]) -> Result<String, ApiError> {
    let term = msgpack::decode(bytes)
        .map_err(|e| ApiError::new(format!("cannot read the input as msgpack: {}", e.msg)))?;
    Ok(render_surface(&term, Surface::Json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_between_surfaces_and_back() {
        let yaml =
            translate(r#"{"name":"w","tags":["a","b"],"n":3}"#, "json", "yaml").expect("to yaml");
        assert!(yaml.contains("name: w"), "{yaml}");
        let json = translate(&yaml, "YAML", " Json ").expect("back to json");
        assert!(json.contains("\"tags\""), "{json}");
    }

    #[test]
    fn msgpack_round_trips_json() {
        let packed = json_to_msgpack(r#"{"a":1,"b":[true,null,"x"]}"#).expect("pack");
        assert_eq!(packed[0] & 0xf0, 0x80, "a msgpack map");
        let json = msgpack_to_json(&packed).expect("unpack");
        assert!(json.contains("\"b\":[true,null,\"x\"]"), "{json}");
    }

    #[test]
    fn failures_say_what_was_rejected() {
        let e = translate("{", "json", "yaml").expect_err("bad json");
        assert!(
            e.to_string().starts_with("cannot read the input as json"),
            "{e}"
        );
        let e = translate("{}", "json", "nope").expect_err("bad format");
        assert!(e.to_string().contains("expected one of json, yaml"), "{e}");
        assert!(msgpack_to_json(&[0xc1]).is_err());
        assert!(msgpack_to_json(&[]).is_err());
    }

    #[test]
    fn every_listed_format_is_accepted() {
        for f in FORMATS {
            assert!(surface(f).is_ok(), "{f}");
        }
    }
}
