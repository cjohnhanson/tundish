//! The Python module, `tundish._native`. It calls the same `publish`
//! and the same CLI as the binary.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use crate::config::Config;
use crate::store::Store;

fn err(e: crate::Error) -> PyErr {
    match e {
        crate::Error::Usage(_) | crate::Error::Json(_) => PyValueError::new_err(e.to_string()),
        _ => PyRuntimeError::new_err(e.to_string()),
    }
}

/// Publish one item; return its id. `payload` is anything `json.dumps`
/// accepts. The database comes from the tundish config; `db` and
/// `config` exist for tests.
#[pyfunction]
#[pyo3(signature = (queue, payload, key=None, db=None, config=None))]
fn publish(
    py: Python<'_>,
    queue: &str,
    payload: Bound<'_, PyAny>,
    key: Option<&str>,
    db: Option<PathBuf>,
    config: Option<PathBuf>,
) -> PyResult<i64> {
    let text: String = py
        .import("json")?
        .call_method1("dumps", (payload,))?
        .extract()?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| err(e.into()))?;
    let cfg = Config::load(config.as_deref()).map_err(err)?;
    let path = db.unwrap_or_else(|| cfg.database.clone());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    py.detach(|| {
        let mut store = Store::open(&path)?;
        store.publish(queue, key, &value, now, cfg.retention(queue))
    })
    .map_err(err)
}

/// Run the CLI on `argv` (program name first); return the exit code.
#[pyfunction]
fn run_cli(py: Python<'_>, argv: Vec<String>) -> i32 {
    py.detach(|| crate::cli::run(argv))
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(publish, m)?)?;
    m.add_function(wrap_pyfunction!(run_cli, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
