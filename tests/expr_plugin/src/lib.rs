//! Minimal polars expression plugin for the `polars_dyn call` tests: the ABI of
//! `pyo3-polars`' `#[polars_expr]` written by hand on top of `polars-ffi` (version 0.1).
//!
//! Exports one function, `add`: takes one Int64 column and the keyword argument `n`, and
//! returns the column with `n` added to every value. The output field keeps the input's name
//! and dtype.

use std::cell::RefCell;
use std::ffi::{CString, c_char};
use std::panic::catch_unwind;

use polars_arrow::ffi::{ArrowSchema, export_field_to_c, import_field_from_c};
use polars_core::prelude::{CompatLevel, Field, IntoSeries, PolarsError, PolarsResult, Series};
use polars_ffi::version_0::{CallerContext, SeriesExport, export_series, import_series_buffer};
use serde::Deserialize;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_last_error(message: &str) {
    let message = CString::new(message.replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|last| *last.borrow_mut() = message);
}

/// Keyword arguments of `add`, decoded from the pickle bytes the caller passes.
#[derive(Deserialize)]
struct AddKwargs {
    n: i64,
}

fn parse_kwargs(kwargs: &[u8]) -> PolarsResult<AddKwargs> {
    serde_pickle::from_slice(kwargs, serde_pickle::DeOptions::new())
        .map_err(|e| PolarsError::InvalidOperation(format!("could not parse kwargs: {e}").into()))
}

fn add(inputs: &[Series], kwargs: &AddKwargs) -> PolarsResult<Series> {
    let [input] = inputs else {
        return Err(PolarsError::InvalidOperation(
            format!("add expects 1 input, got {}", inputs.len()).into(),
        ));
    };
    Ok((input.i64()? + kwargs.n).into_series())
}

fn add_field(inputs: &[Field]) -> PolarsResult<Field> {
    let [input] = inputs else {
        return Err(PolarsError::InvalidOperation(
            format!("add expects 1 input, got {}", inputs.len()).into(),
        ));
    };
    Ok(input.clone())
}

/// polars-ffi version 0.1 as `major << 16 | minor`.
#[unsafe(no_mangle)]
pub extern "C" fn _polars_plugin_get_version() -> u32 {
    let (major, minor) = polars_ffi::get_version();
    (u32::from(major) << 16) | u32::from(minor)
}

/// Returns the last error message of this thread as a NUL-terminated string. polars copies it
/// right away, before the next call can replace it.
#[unsafe(no_mangle)]
pub extern "C" fn _polars_plugin_get_last_error_message() -> *const c_char {
    LAST_ERROR.with(|last| last.borrow().as_ptr())
}

/// # Safety
/// `fields` must point to `len` valid `ArrowSchema`s, `return_value` to writable memory, and
/// `kwargs_ptr` to `kwargs_len` readable bytes, all for the duration of the call (polars owns
/// and frees them).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _polars_plugin_field_add(
    fields: *const ArrowSchema,
    len: usize,
    return_value: *mut ArrowSchema,
    kwargs_ptr: *const u8,
    kwargs_len: usize,
) {
    let result = catch_unwind(|| {
        // SAFETY: the caller guarantees the two slices per the contract above.
        let (fields, kwargs) = unsafe {
            (
                std::slice::from_raw_parts(fields, len),
                std::slice::from_raw_parts(kwargs_ptr, kwargs_len),
            )
        };
        parse_kwargs(kwargs)?;
        let fields = fields
            .iter()
            // SAFETY: each schema is a valid `ArrowSchema` per the contract above.
            .map(|schema| unsafe { import_field_from_c(schema) }.map(|f| Field::from(&f)))
            .collect::<PolarsResult<Vec<_>>>()?;
        let out = add_field(&fields)?;
        Ok::<_, PolarsError>(export_field_to_c(&out.to_arrow(CompatLevel::newest())))
    });
    match result {
        // SAFETY: `return_value` is writable per the contract above.
        Ok(Ok(schema)) => unsafe { *return_value = schema },
        Ok(Err(e)) => set_last_error(&e.to_string()),
        Err(_) => set_last_error("PANIC"),
    }
}

/// # Safety
/// `inputs` must point to `len` valid `SeriesExport`s whose ownership passes to this function,
/// `return_value` to writable memory, and `kwargs_ptr` to `kwargs_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _polars_plugin_add(
    inputs: *const SeriesExport,
    len: usize,
    kwargs_ptr: *const u8,
    kwargs_len: usize,
    return_value: *mut SeriesExport,
    _context: *const CallerContext,
) {
    let result = catch_unwind(|| {
        // SAFETY: the caller hands over `len` exports and a readable kwargs slice per the
        // contract above. `import_series_buffer` takes ownership of the exports.
        let (series, kwargs) = unsafe {
            (
                import_series_buffer(inputs.cast_mut(), len)?,
                std::slice::from_raw_parts(kwargs_ptr, kwargs_len),
            )
        };
        let kwargs = parse_kwargs(kwargs)?;
        add(&series, &kwargs)
    });
    match result {
        // SAFETY: `return_value` is writable per the contract above.
        Ok(Ok(series)) => unsafe { *return_value = export_series(&series) },
        Ok(Err(e)) => set_last_error(&e.to_string()),
        Err(_) => set_last_error("PANIC"),
    }
}
