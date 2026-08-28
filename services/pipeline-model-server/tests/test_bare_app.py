import httpx
import numpy as np
import unittest

from uparser_pipeline_server.bare_app import CONTENT_TYPE, create_app
from uparser_pipeline_server.bare_models import BareModel, BareModelRegistry
from uparser_pipeline_server.tensor_wire import TensorBundle, decode, encode


def registry_with_echo() -> BareModelRegistry:
    registry = BareModelRegistry()
    registry.register(
        BareModel(
            name="echo",
            revision="test",
            input_schema={"input": {"dtype": "f32", "shape": ["batch"]}},
            output_schema={"output": {"dtype": "f32", "shape": ["batch"]}},
            infer=lambda bundle: TensorBundle(
                tensors={"output": bundle.tensors["input"] + 1},
                metadata={"model": "echo"},
            ),
        )
    )
    return registry


class BareAppTests(unittest.IsolatedAsyncioTestCase):
    async def request(self, app, method, path, **kwargs):
        transport = httpx.ASGITransport(app=app)
        async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
            return await client.request(method, path, **kwargs)

    async def test_roundtrips_binary_tensors_without_business_schema(self):
        request = encode(
            TensorBundle(tensors={"input": np.asarray([1.0, 2.0], dtype=np.float32)})
        )

        response = await self.request(
            create_app(registry_with_echo()),
            "POST",
            "/v1/models/echo:infer",
            content=request,
            headers={"content-type": CONTENT_TYPE},
        )

        self.assertEqual(response.status_code, 200)
        output = decode(response.content)
        np.testing.assert_array_equal(output.tensors["output"], [2.0, 3.0])
        self.assertEqual(output.metadata, {"model": "echo"})

    async def test_rejects_json_requests(self):
        response = await self.request(
            create_app(registry_with_echo()),
            "POST",
            "/v1/models/echo:infer",
            json={"page": "not allowed"},
        )

        self.assertEqual(response.status_code, 415)
