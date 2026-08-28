import numpy as np
import pytest

from uparser_pipeline_server.tensor_wire import TensorBundle, decode, encode


def test_roundtrip_preserves_tensor_shapes_dtypes_and_metadata():
    bundle = TensorBundle(
        tensors={
            "pixels": np.asarray([[1.0, 2.0]], dtype=np.float32),
            "mask": np.asarray([1, 0], dtype=np.uint8),
        },
        metadata={"model": "layout", "revision": "test"},
    )

    decoded = decode(encode(bundle))

    assert decoded.metadata == bundle.metadata
    np.testing.assert_array_equal(decoded.tensors["pixels"], bundle.tensors["pixels"])
    np.testing.assert_array_equal(decoded.tensors["mask"], bundle.tensors["mask"])


def test_rejects_trailing_payload_bytes():
    encoded = encode(TensorBundle(tensors={"value": np.asarray([1], dtype=np.int32)}))

    with pytest.raises(ValueError, match="unreferenced"):
        decode(encoded + b"x")
