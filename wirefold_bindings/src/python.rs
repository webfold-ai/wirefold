//! The `wirefold` Python module (PyO3).

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::api;

fn err(e: api::ApiError) -> PyErr {
    PyValueError::new_err(e.to_string())
}

/// Convert a document between text surfaces.
#[pyfunction]
#[pyo3(signature = (input, from_format, to_format))]
fn translate(input: &str, from_format: &str, to_format: &str) -> PyResult<String> {
    api::translate(input, from_format, to_format).map_err(err)
}

/// JSON text to MessagePack bytes.
#[pyfunction]
fn json_to_msgpack<'py>(py: Python<'py>, json: &str) -> PyResult<Bound<'py, PyBytes>> {
    api::json_to_msgpack(json)
        .map(|b| PyBytes::new(py, &b))
        .map_err(err)
}

/// MessagePack bytes to JSON text.
#[pyfunction]
fn msgpack_to_json(data: &[u8]) -> PyResult<String> {
    api::msgpack_to_json(data).map_err(err)
}

/// The surface names.
#[pyfunction]
fn formats() -> Vec<&'static str> {
    api::FORMATS.to_vec()
}

/// The version of the bindings.
#[pyfunction]
fn version() -> &'static str {
    api::version()
}

#[pymodule]
#[pyo3(name = "wirefold")]
fn wirefold_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(translate, m)?)?;
    m.add_function(wrap_pyfunction!(json_to_msgpack, m)?)?;
    m.add_function(wrap_pyfunction!(msgpack_to_json, m)?)?;
    m.add_function(wrap_pyfunction!(formats, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    Ok(())
}
