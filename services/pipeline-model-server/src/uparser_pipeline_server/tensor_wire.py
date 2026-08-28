"""Binary tensor envelope for bare model inference requests."""

from __future__ import annotations

import json
import struct
from dataclasses import dataclass, field

import numpy as np


MAGIC = b"UPTENSOR"
VERSION = 1
HEADER = struct.Struct("<8sHI")
MAX_MANIFEST_BYTES = 1024 * 1024
DTYPES = {
    "f32": np.dtype("<f4"),
    "f16": np.dtype("<f2"),
    "i64": np.dtype("<i8"),
    "i32": np.dtype("<i4"),
    "u8": np.dtype("u1"),
}


@dataclass(frozen=True)
class TensorBundle:
    tensors: dict[str, np.ndarray]
    metadata: dict[str, str] = field(default_factory=dict)


def encode(bundle: TensorBundle) -> bytes:
    descriptors = []
    chunks = []
    offset = 0
    for name, value in bundle.tensors.items():
        if not name:
            raise ValueError("tensor name must not be empty")
        array = np.ascontiguousarray(value)
        dtype_name = next((name for name, dtype in DTYPES.items() if array.dtype == dtype), None)
        if dtype_name is None:
            raise ValueError(f"unsupported tensor dtype: {array.dtype}")
        data = array.tobytes(order="C")
        descriptors.append(
            {
                "name": name,
                "dtype": dtype_name,
                "shape": list(array.shape),
                "offset": offset,
                "length": len(data),
            }
        )
        chunks.append(data)
        offset += len(data)
    manifest = json.dumps(
        {"metadata": bundle.metadata, "tensors": descriptors},
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    if len(manifest) > MAX_MANIFEST_BYTES:
        raise ValueError("tensor manifest exceeds the configured limit")
    return HEADER.pack(MAGIC, VERSION, len(manifest)) + manifest + b"".join(chunks)


def decode(encoded: bytes) -> TensorBundle:
    if len(encoded) < HEADER.size:
        raise ValueError("tensor envelope is shorter than its header")
    magic, version, manifest_len = HEADER.unpack_from(encoded)
    if magic != MAGIC:
        raise ValueError("invalid tensor envelope magic")
    if version != VERSION:
        raise ValueError(f"unsupported tensor envelope version {version}")
    if manifest_len > MAX_MANIFEST_BYTES:
        raise ValueError("tensor manifest exceeds the configured limit")
    payload_start = HEADER.size + manifest_len
    if payload_start > len(encoded):
        raise ValueError("tensor manifest is truncated")
    manifest = json.loads(encoded[HEADER.size:payload_start])
    payload = memoryview(encoded)[payload_start:]
    tensors = {}
    next_offset = 0
    for descriptor in manifest["tensors"]:
        name = descriptor["name"]
        if not name or name in tensors:
            raise ValueError(f"invalid or duplicate tensor name: {name}")
        dtype = DTYPES.get(descriptor["dtype"])
        if dtype is None:
            raise ValueError(f"unsupported tensor dtype: {descriptor['dtype']}")
        shape = tuple(descriptor["shape"])
        offset = descriptor["offset"]
        length = descriptor["length"]
        if offset != next_offset or offset < 0 or length < 0 or offset + length > len(payload):
            raise ValueError("tensor payload is non-canonical or out of bounds")
        expected = int(np.prod(shape, dtype=np.int64)) * dtype.itemsize
        if expected != length:
            raise ValueError(f"tensor {name} byte length is {length}, expected {expected}")
        tensors[name] = np.frombuffer(payload[offset : offset + length], dtype=dtype).reshape(shape)
        next_offset = offset + length
    if next_offset != len(payload):
        raise ValueError("tensor payload contains unreferenced bytes")
    metadata = manifest.get("metadata", {})
    if not isinstance(metadata, dict) or not all(
        isinstance(key, str) and isinstance(value, str) for key, value in metadata.items()
    ):
        raise ValueError("tensor metadata must contain string keys and values")
    return TensorBundle(tensors=tensors, metadata=metadata)
