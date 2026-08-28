//! Binary tensor envelope shared with the bare model service.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

const MAGIC: &[u8; 8] = b"UPTENSOR";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 14;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TensorDType {
    F32,
    F16,
    I64,
    I32,
    U8,
}

impl TensorDType {
    fn width(self) -> usize {
        match self {
            Self::F32 | Self::I32 => 4,
            Self::F16 => 2,
            Self::I64 => 8,
            Self::U8 => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tensor {
    pub name: String,
    pub dtype: TensorDType,
    pub shape: Vec<usize>,
    pub data: Vec<u8>,
}

impl Tensor {
    pub fn from_f32(name: impl Into<String>, shape: Vec<usize>, values: &[f32]) -> Self {
        Self {
            name: name.into(),
            dtype: TensorDType::F32,
            shape,
            data: values
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect(),
        }
    }

    pub fn to_f32(&self) -> Result<Vec<f32>, TensorWireError> {
        if self.dtype != TensorDType::F32 {
            return Err(TensorWireError::UnexpectedDType {
                name: self.name.clone(),
                expected: TensorDType::F32,
                actual: self.dtype,
            });
        }
        Ok(self
            .data
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte chunk")))
            .collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TensorBundle {
    pub metadata: BTreeMap<String, String>,
    pub tensors: Vec<Tensor>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    metadata: BTreeMap<String, String>,
    tensors: Vec<TensorDescriptor>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TensorDescriptor {
    name: String,
    dtype: TensorDType,
    shape: Vec<usize>,
    offset: usize,
    length: usize,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TensorWireError {
    #[error("tensor envelope is shorter than its header")]
    ShortHeader,
    #[error("invalid tensor envelope magic")]
    InvalidMagic,
    #[error("unsupported tensor envelope version {0}")]
    UnsupportedVersion(u16),
    #[error("tensor manifest exceeds the configured limit")]
    ManifestTooLarge,
    #[error("tensor manifest is truncated")]
    TruncatedManifest,
    #[error("invalid tensor manifest: {0}")]
    InvalidManifest(String),
    #[error("tensor name must not be empty")]
    EmptyName,
    #[error("duplicate tensor name: {0}")]
    DuplicateName(String),
    #[error("tensor {name} shape overflows addressable memory")]
    ShapeOverflow { name: String },
    #[error("tensor {name} byte length is {actual}, expected {expected}")]
    InvalidLength {
        name: String,
        actual: usize,
        expected: usize,
    },
    #[error("tensor {0} points outside the payload")]
    OutOfBounds(String),
    #[error("tensor {name} has dtype {actual:?}, expected {expected:?}")]
    UnexpectedDType {
        name: String,
        expected: TensorDType,
        actual: TensorDType,
    },
    #[error("tensor payload contains unreferenced or overlapping bytes")]
    NonCanonicalPayload,
}

fn expected_len(tensor: &Tensor) -> Result<usize, TensorWireError> {
    tensor
        .shape
        .iter()
        .try_fold(tensor.dtype.width(), |bytes, dimension| {
            bytes.checked_mul(*dimension)
        })
        .ok_or_else(|| TensorWireError::ShapeOverflow {
            name: tensor.name.clone(),
        })
}

fn validate_tensor(tensor: &Tensor, names: &mut HashSet<String>) -> Result<(), TensorWireError> {
    if tensor.name.is_empty() {
        return Err(TensorWireError::EmptyName);
    }
    if !names.insert(tensor.name.clone()) {
        return Err(TensorWireError::DuplicateName(tensor.name.clone()));
    }
    let expected = expected_len(tensor)?;
    if tensor.data.len() != expected {
        return Err(TensorWireError::InvalidLength {
            name: tensor.name.clone(),
            actual: tensor.data.len(),
            expected,
        });
    }
    Ok(())
}

pub fn encode(bundle: &TensorBundle) -> Result<Vec<u8>, TensorWireError> {
    let mut names = HashSet::new();
    let mut offset = 0usize;
    let mut descriptors = Vec::with_capacity(bundle.tensors.len());
    for tensor in &bundle.tensors {
        validate_tensor(tensor, &mut names)?;
        descriptors.push(TensorDescriptor {
            name: tensor.name.clone(),
            dtype: tensor.dtype,
            shape: tensor.shape.clone(),
            offset,
            length: tensor.data.len(),
        });
        offset = offset.checked_add(tensor.data.len()).ok_or_else(|| {
            TensorWireError::ShapeOverflow {
                name: tensor.name.clone(),
            }
        })?;
    }
    let manifest = serde_json::to_vec(&Manifest {
        metadata: bundle.metadata.clone(),
        tensors: descriptors,
    })
    .map_err(|error| TensorWireError::InvalidManifest(error.to_string()))?;
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(TensorWireError::ManifestTooLarge);
    }

    let mut encoded = Vec::with_capacity(HEADER_LEN + manifest.len() + offset);
    encoded.extend_from_slice(MAGIC);
    encoded.extend_from_slice(&VERSION.to_le_bytes());
    encoded.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
    encoded.extend_from_slice(&manifest);
    for tensor in &bundle.tensors {
        encoded.extend_from_slice(&tensor.data);
    }
    Ok(encoded)
}

pub fn decode(encoded: &[u8]) -> Result<TensorBundle, TensorWireError> {
    if encoded.len() < HEADER_LEN {
        return Err(TensorWireError::ShortHeader);
    }
    if &encoded[..MAGIC.len()] != MAGIC {
        return Err(TensorWireError::InvalidMagic);
    }
    let version = u16::from_le_bytes([encoded[8], encoded[9]]);
    if version != VERSION {
        return Err(TensorWireError::UnsupportedVersion(version));
    }
    let manifest_len =
        u32::from_le_bytes(encoded[10..14].try_into().expect("fixed header")) as usize;
    if manifest_len > MAX_MANIFEST_BYTES {
        return Err(TensorWireError::ManifestTooLarge);
    }
    let payload_start = HEADER_LEN
        .checked_add(manifest_len)
        .ok_or(TensorWireError::TruncatedManifest)?;
    if payload_start > encoded.len() {
        return Err(TensorWireError::TruncatedManifest);
    }
    let manifest: Manifest = serde_json::from_slice(&encoded[HEADER_LEN..payload_start])
        .map_err(|error| TensorWireError::InvalidManifest(error.to_string()))?;
    let payload = &encoded[payload_start..];
    let mut names = HashSet::new();
    let mut next_offset = 0usize;
    let mut tensors = Vec::with_capacity(manifest.tensors.len());
    for descriptor in manifest.tensors {
        let end = descriptor
            .offset
            .checked_add(descriptor.length)
            .ok_or_else(|| TensorWireError::OutOfBounds(descriptor.name.clone()))?;
        if end > payload.len() {
            return Err(TensorWireError::OutOfBounds(descriptor.name));
        }
        if descriptor.offset != next_offset {
            return Err(TensorWireError::NonCanonicalPayload);
        }
        let tensor = Tensor {
            name: descriptor.name,
            dtype: descriptor.dtype,
            shape: descriptor.shape,
            data: payload[descriptor.offset..end].to_vec(),
        };
        validate_tensor(&tensor, &mut names)?;
        next_offset = end;
        tensors.push(tensor);
    }
    if next_offset != payload.len() {
        return Err(TensorWireError::NonCanonicalPayload);
    }
    Ok(TensorBundle {
        metadata: manifest.metadata,
        tensors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TensorBundle {
        TensorBundle {
            metadata: BTreeMap::from([
                ("model".into(), "layout".into()),
                ("revision".into(), "test".into()),
            ]),
            tensors: vec![
                Tensor {
                    name: "pixels".into(),
                    dtype: TensorDType::F32,
                    shape: vec![1, 2],
                    data: [1.0f32.to_le_bytes(), 2.0f32.to_le_bytes()].concat(),
                },
                Tensor {
                    name: "mask".into(),
                    dtype: TensorDType::U8,
                    shape: vec![2],
                    data: vec![1, 0],
                },
            ],
        }
    }

    #[test]
    fn roundtrips_multiple_tensors_without_copying_business_schema() {
        let bundle = sample();
        assert_eq!(decode(&encode(&bundle).unwrap()).unwrap(), bundle);
    }

    #[test]
    fn rejects_shape_byte_length_mismatch() {
        let mut bundle = sample();
        bundle.tensors[0].shape = vec![3];
        assert!(matches!(
            encode(&bundle),
            Err(TensorWireError::InvalidLength { .. })
        ));
    }

    #[test]
    fn rejects_trailing_payload_bytes() {
        let mut encoded = encode(&sample()).unwrap();
        encoded.push(0);
        assert_eq!(decode(&encoded), Err(TensorWireError::NonCanonicalPayload));
    }

    #[test]
    fn f32_helpers_use_little_endian_wire_values() {
        let tensor = Tensor::from_f32("values", vec![2], &[1.25, -2.5]);
        assert_eq!(tensor.to_f32().unwrap(), vec![1.25, -2.5]);
    }
}
