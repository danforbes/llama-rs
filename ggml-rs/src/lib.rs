#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(unused)]

//! `ggml-rs` is a semi-idiomatic wrapper for the `ggml` C library.
//!
//! It exposes a subset of operations (currently used to implement the [llama-rs](https://crates.io/crates/llama-rs) library).
//! Note that it does not expose a fully-idiomatic safe Rust interface; operations that could be potentially unsafe are marked as such.
//!
//! `ggml-rs` operates on a computational graph; no values will be computed until [Context::graph_compute] is executed.
//! All [Tensor]s are nodes in this computational graph, and values cannot be retrieved until computation is completed.

use std::{
    os::raw::{c_int, c_void},
    ptr::NonNull,
    sync::{Arc, Weak},
};

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

pub use tensor::Tensor;

/// Utilities for reading and writing.
pub mod util;

pub mod loader;

pub mod saver;

pub mod context;
mod tensor;

#[cfg(test)]
mod tests;

/// The type of a tensor element.
pub type ElementType = Type;

#[derive(Debug, PartialEq, Clone, Copy)]
/// The format of the file containing the model.
pub enum ContainerType {
    /// `GGML`: legacy format, oldest ggml tensor file format
    Ggml,
    /// `GGMF`: also legacy format. Introduces versioning. Newer than GGML, older than GGJT.
    Ggmf,
    /// `GGJT`: mmap-able format.
    Ggjt,
}
impl ContainerType {
    /// Does this container type support mmap?
    pub fn support_mmap(&self) -> bool {
        match self {
            ContainerType::Ggml => false,
            ContainerType::Ggmf => false,
            ContainerType::Ggjt => true,
        }
    }
}

/// Magic constant for `ggml` files (versioned, ggmf).
pub const FILE_MAGIC_GGMF: u32 = 0x67676d66;
/// Magic constant for `ggml` files (versioned, ggjt).
pub const FILE_MAGIC_GGJT: u32 = 0x67676a74;
/// Magic constant for `ggml` files (unversioned).
pub const FILE_MAGIC_UNVERSIONED: u32 = 0x67676d6c;

/// The currently-supported format version for `ggml` files.
pub const FORMAT_VERSION: u32 = 1;

/// The size of a `ggml` object.
pub const OBJECT_SIZE: usize = crate::GGML_OBJECT_SIZE;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
/// The type of a value in `ggml`.
pub enum Type {
    /// Quantized 4-bit (type 0).
    #[default]
    Q4_0,
    /// Quantized 4-bit (type 1); used by GPTQ.
    Q4_1,
    /// Quantized 4-bit (type 2).
    Q4_2,
    /// Quantized 4-bit (type 3).
    Q4_3,
    /// Quantized 8-bit (type 0).
    Q8_0,
    /// Integer 32-bit.
    I32,
    /// Float 16-bit.
    F16,
    /// Float 32-bit.
    F32,
}
impl From<Type> for crate::ggml_type {
    fn from(t: Type) -> Self {
        match t {
            Type::Q4_0 => crate::ggml_type_GGML_TYPE_Q4_0,
            Type::Q4_1 => crate::ggml_type_GGML_TYPE_Q4_1,
            Type::Q4_2 => crate::ggml_type_GGML_TYPE_Q4_2,
            Type::Q4_3 => crate::ggml_type_GGML_TYPE_Q4_3,
            Type::Q8_0 => crate::ggml_type_GGML_TYPE_Q8_0,
            Type::I32 => crate::ggml_type_GGML_TYPE_I32,
            Type::F16 => crate::ggml_type_GGML_TYPE_F16,
            Type::F32 => crate::ggml_type_GGML_TYPE_F32,
        }
    }
}
impl TryFrom<crate::ggml_type> for Type {
    type Error = ();
    fn try_from(t: crate::ggml_type) -> Result<Self, Self::Error> {
        match t {
            crate::ggml_type_GGML_TYPE_Q4_0 => Ok(Type::Q4_0),
            crate::ggml_type_GGML_TYPE_Q4_1 => Ok(Type::Q4_1),
            crate::ggml_type_GGML_TYPE_Q4_2 => Ok(Type::Q4_2),
            crate::ggml_type_GGML_TYPE_Q4_3 => Ok(Type::Q4_3),
            crate::ggml_type_GGML_TYPE_Q8_0 => Ok(Type::Q8_0),
            crate::ggml_type_GGML_TYPE_I32 => Ok(Type::I32),
            crate::ggml_type_GGML_TYPE_F16 => Ok(Type::F16),
            crate::ggml_type_GGML_TYPE_F32 => Ok(Type::F32),
            _ => Err(()),
        }
    }
}
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Q4_0 => write!(f, "q4_0"),
            Type::Q4_1 => write!(f, "q4_1"),
            Type::Q4_2 => write!(f, "q4_2"),
            Type::Q4_3 => write!(f, "q4_3"),
            Type::Q8_0 => write!(f, "q8_0"),
            Type::I32 => write!(f, "i32"),
            Type::F16 => write!(f, "f16"),
            Type::F32 => write!(f, "f32"),
        }
    }
}

/// A buffer of memory that can be used as a scratch buffer for a [Context].
///
/// See [Context::use_scratch].
pub struct Buffer {
    data: Box<[u8]>,
}

impl Buffer {
    /// Creates a new buffer of the specified size.
    pub fn new(size: usize) -> Self {
        let mut data: Vec<u8> = Vec::with_capacity(size);

        // SAFETY: The contents are intentionally uninitialized, as they will be passed to
        // the ggml C API which will fill them with data.
        #[allow(clippy::uninit_vec)]
        unsafe {
            data.set_len(size);
        }

        Buffer {
            data: data.into_boxed_slice(),
        }
    }
}

/// A `ggml` computation graph. Keeps track of all state during computation.
pub struct ComputationGraph {
    inner: crate::ggml_cgraph,
}

impl ComputationGraph {
    /// Create a new [ComputationGraph] with the specified `n_threads`.
    pub fn new(n_threads: usize) -> Self {
        Self {
            inner: crate::ggml_cgraph {
                n_threads: usize_to_i32(n_threads),
                // SAFETY: This should be safe to zero. The original C++ impl
                // just leaves it uninitialized
                ..unsafe { std::mem::zeroed::<crate::ggml_cgraph>() }
            },
        }
    }

    /// Build this computational graph in the forward direction in preparation for computation.
    pub fn build_forward_expand(&mut self, tensor: &Tensor) {
        unsafe { crate::ggml_build_forward_expand(&mut self.inner, tensor.ptr.as_ptr()) }
    }
}

/// The size of `t` as bytes.
pub fn type_size(t: Type) -> usize {
    unsafe { crate::ggml_type_size(t.into()) }
}

/// [type_size]/[blck_size] as float.
pub fn type_sizef(x: Type) -> f64 {
    (unsafe { crate::ggml_type_sizef(x.into()) }) as f64
}

/// The size of a block for `t`. Only relevant for quantized types.
pub fn blck_size(t: Type) -> usize {
    i32_to_usize(unsafe { crate::ggml_blck_size(t.into()) })
}

fn usize_to_i32(val: usize) -> i32 {
    i32::try_from(val).unwrap()
}

fn usize_to_i64(val: usize) -> i64 {
    i64::try_from(val).unwrap()
}

fn i32_to_usize(val: i32) -> usize {
    usize::try_from(val).unwrap()
}

fn i64_to_usize(val: i64) -> usize {
    usize::try_from(val).unwrap()
}

/// Contains the result of a quantization operation.
pub struct QuantizationResult {
    /// The quantized output.
    pub output: Vec<u8>,
    /// The quantization history.
    pub history: Vec<i64>,
}

/// Quantizes `src` into `dst` using `q4_0` quantization.
///
/// You must ensure that `src.len() == n_elements`, and `n_elements_0`
/// is the first dimension of `src`.
pub fn quantize_q4_0(src: &[f32], n_elements: usize, n_elements_0: usize) -> QuantizationResult {
    quantize_impl(src, n_elements, n_elements_0, crate::ggml_quantize_q4_0)
}

/// Quantizes `src` into `dst` using `q4_1` quantization.
///
/// You must ensure that `src.len() == n_elements`, and `n_elements_0`
/// is the first dimension of `src`.
pub fn quantize_q4_1(src: &[f32], n_elements: usize, n_elements_0: usize) -> QuantizationResult {
    quantize_impl(src, n_elements, n_elements_0, crate::ggml_quantize_q4_1)
}

fn quantize_impl(
    src: &[f32],
    n_elements: usize,
    n_elements_0: usize,
    quantizer: unsafe extern "C" fn(*const f32, *mut c_void, c_int, c_int, *mut i64) -> usize,
) -> QuantizationResult {
    assert_eq!(src.len(), n_elements);
    assert_eq!(n_elements % n_elements_0, 0);

    // A conservative multiplier of 4 is used here.
    let mut output = vec![0u8; n_elements * 4];
    let mut history = vec![0i64; 16];
    let output_size = unsafe {
        quantizer(
            src.as_ptr(),
            output.as_mut_ptr() as *mut c_void,
            n_elements.try_into().unwrap(),
            n_elements_0.try_into().unwrap(),
            history.as_mut_ptr(),
        )
    };

    output.resize(output_size, 0u8);
    QuantizationResult { output, history }
}