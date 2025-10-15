use pyo3::prelude::*;

#[pyfunction]
pub fn rpc_method(func: PyObject) -> PyResult<PyObject> {
    Python::with_gil(|py| {
        func.setattr(py, "_is_rpc_method", true)?;
        func.setattr(py, "_is_stream_method", false)?;
        Ok(func)
    })
}

#[pyfunction]
pub fn rpc_stream(func: PyObject) -> PyResult<PyObject> {
    Python::with_gil(|py| {
        func.setattr(py, "_is_rpc_method", true)?;
        func.setattr(py, "_is_stream_method", true)?;
        Ok(func)
    })
}


#[pyfunction]
pub fn rpc_stream_iter(func: PyObject) -> PyResult<PyObject> {
    Python::with_gil(|py| {
        func.setattr(py, "_is_rpc_method", true)?;
        func.setattr(py, "_is_stream_method", true)?;
        func.setattr(py, "_is_stream_iter_method", true)?;
        Ok(func)
    })
}