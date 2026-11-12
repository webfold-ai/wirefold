//! The WebAssembly module (wasm-bindgen). `wasm-bindgen` turns this into JavaScript and TypeScript
//! declarations.

use wasm_bindgen::prelude::*;

use crate::api;

fn err(e: api::ApiError) -> JsError {
    JsError::new(&e.to_string())
}

/// Convert a document between text surfaces.
#[wasm_bindgen]
pub fn translate(input: &str, from: &str, to: &str) -> Result<String, JsError> {
    api::translate(input, from, to).map_err(err)
}

/// JSON text to MessagePack bytes.
#[wasm_bindgen(js_name = jsonToMsgpack)]
pub fn json_to_msgpack(json: &str) -> Result<Vec<u8>, JsError> {
    api::json_to_msgpack(json).map_err(err)
}

/// MessagePack bytes to JSON text.
#[wasm_bindgen(js_name = msgpackToJson)]
pub fn msgpack_to_json(data: &[u8]) -> Result<String, JsError> {
    api::msgpack_to_json(data).map_err(err)
}

/// The surface names.
#[wasm_bindgen]
#[must_use]
pub fn formats() -> Vec<String> {
    api::FORMATS.iter().map(|s| (*s).to_owned()).collect()
}

/// The version of the bindings.
#[wasm_bindgen]
#[must_use]
pub fn version() -> String {
    api::version().to_owned()
}
