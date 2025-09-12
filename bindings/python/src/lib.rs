use pyo3::{prelude::*};
pub mod core;
pub mod decorators;
use crate::core::{LatticaSDK, RpcClient, PeerInfo};
use crate::decorators::{rpc_method, rpc_stream};

#[pymodule]
fn lattica_python_core(_py: Python, m: &Bound<PyModule>) -> PyResult<()> {
    m.add_class::<LatticaSDK>()?;
    m.add_class::<RpcClient>()?;
    m.add_class::<PeerInfo>()?;

    m.add_function(wrap_pyfunction!(rpc_method, m)?)?;
    m.add_function(wrap_pyfunction!(rpc_stream, m)?)?;
    Ok(())
}