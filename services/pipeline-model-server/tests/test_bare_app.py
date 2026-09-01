import asyncio
import httpx
import numpy as np
import threading
import unittest
from unittest.mock import patch

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

    async def test_independent_models_can_run_concurrently(self):
        barrier = threading.Barrier(2, timeout=2)
        registry = BareModelRegistry()
        for name in ("left", "right"):
            registry.register(
                BareModel(
                    name=name,
                    revision="test",
                    input_schema={},
                    output_schema={},
                    infer=lambda bundle: (barrier.wait(), bundle)[1],
                )
            )
        app = create_app(registry)
        body = encode(TensorBundle(tensors={"input": np.asarray([1], dtype=np.int32)}))

        responses = await asyncio.gather(
            self.request(
                app,
                "POST",
                "/v1/models/left:infer",
                content=body,
                headers={"content-type": CONTENT_TYPE},
            ),
            self.request(
                app,
                "POST",
                "/v1/models/right:infer",
                content=body,
                headers={"content-type": CONTENT_TYPE},
            ),
        )

        self.assertEqual([response.status_code for response in responses], [200, 200])

    async def test_compatible_requests_share_one_model_forward(self):
        forwarded_shapes = []

        def infer(bundle):
            forwarded_shapes.append(bundle.tensors["input"].shape)
            return TensorBundle(
                tensors={"output": bundle.tensors["input"] + 1},
                metadata={"model": "batched"},
            )

        registry = BareModelRegistry()
        registry.register(BareModel("batched", "test", {}, {}, infer))
        app = create_app(registry)

        async def request(value):
            body = encode(
                TensorBundle(tensors={"input": np.asarray([value], dtype=np.float32)})
            )
            return await self.request(
                app,
                "POST",
                "/v1/models/batched:infer",
                content=body,
                headers={"content-type": CONTENT_TYPE},
            )

        responses = await asyncio.gather(request(2.0), request(7.0))

        self.assertEqual(forwarded_shapes, [(2,)])
        self.assertEqual(
            [decode(response.content).tensors["output"].tolist() for response in responses],
            [[3.0], [8.0]],
        )
        health = await self.request(app, "GET", "/health")
        stats = health.json()["inference"]["batched"]
        self.assertEqual(stats["inference_calls"], 1)
        self.assertEqual(stats["inference_items"], 2)
        self.assertEqual(stats["max_batch_items"], 2)
        self.assertGreaterEqual(stats["inference_seconds"], 0)

    async def test_accuracy_sensitive_models_are_not_microbatched(self):
        forwarded_shapes = []

        def infer(bundle):
            forwarded_shapes.append(bundle.tensors["pixel_values"].shape)
            return bundle

        registry = BareModelRegistry()
        registry.register(BareModel("pp_doclayout_v2", "test", {}, {}, infer))
        app = create_app(registry)

        async def request(value):
            body = encode(
                TensorBundle(
                    tensors={"pixel_values": np.asarray([[value]], dtype=np.float32)}
                )
            )
            return await self.request(
                app,
                "POST",
                "/v1/models/pp_doclayout_v2:infer",
                content=body,
                headers={"content-type": CONTENT_TYPE},
            )

        responses = await asyncio.gather(request(2.0), request(7.0))

        self.assertEqual([response.status_code for response in responses], [200, 200])
        self.assertEqual(forwarded_shapes, [(1, 1), (1, 1)])

    async def test_accuracy_sensitive_batching_requires_explicit_experiment_flag(self):
        forwarded_shapes = []

        def infer(bundle):
            forwarded_shapes.append(bundle.tensors["pixel_values"].shape)
            return bundle

        registry = BareModelRegistry()
        registry.register(BareModel("pp_doclayout_v2", "test", {}, {}, infer))
        app = create_app(registry)

        async def request(value):
            body = encode(
                TensorBundle(
                    tensors={"pixel_values": np.asarray([[value]], dtype=np.float32)}
                )
            )
            return await self.request(
                app,
                "POST",
                "/v1/models/pp_doclayout_v2:infer",
                content=body,
                headers={"content-type": CONTENT_TYPE},
            )

        with patch.dict(
            "os.environ",
            {"UPARSER_BARE_FORCE_BATCH_MODELS": "pp_doclayout_v2"},
            clear=False,
        ):
            responses = await asyncio.gather(request(2.0), request(7.0))

        self.assertEqual([response.status_code for response in responses], [200, 200])
        self.assertEqual(forwarded_shapes, [(2, 1)])
