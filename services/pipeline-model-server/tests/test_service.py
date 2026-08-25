import threading
import unittest

import httpx

from uparser_pipeline_server.app import create_app
from uparser_pipeline_server.registry import BackendRegistry, ModelRegistry, RegisteredBackend
from uparser_pipeline_server.schemas import LayoutResult, ModelMetadata


def metadata(name="fixture"):
    return ModelMetadata(name=name, revision="test", runtime="unit-test")


def page_payload(page_id="page-1"):
    return {
        "page_id": page_id,
        "image": {"media_type": "image/png", "base64_data": "cG5n"},
        "dimensions": {"width": 100, "height": 200},
        "rotation_degrees": 0,
    }


class ModelRegistryTests(unittest.TestCase):
    def test_concurrent_get_or_load_initializes_once(self):
        registry = ModelRegistry()
        load_count = 0
        load_lock = threading.Lock()

        def loader():
            nonlocal load_count
            with load_lock:
                load_count += 1
            return object()

        values = []
        threads = [
            threading.Thread(target=lambda: values.append(registry.get_or_load(("ocr", "ch"), loader)))
            for _ in range(12)
        ]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

        self.assertEqual(load_count, 1)
        self.assertEqual(len({id(value) for value in values}), 1)


class ServiceTests(unittest.IsolatedAsyncioTestCase):
    async def request(self, app, method, path, **kwargs):
        transport = httpx.ASGITransport(app=app)
        async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
            return await client.request(method, path, **kwargs)

    async def test_health_is_not_ready_without_real_backends(self):
        response = await self.request(create_app(), "GET", "/health")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["status"], "not_ready")
        self.assertIn("layout", response.json()["missing_stages"])

    async def test_unregistered_backend_is_loud(self):
        response = await self.request(
            create_app(),
            "POST",
            "/v2/pipeline/layout:batch",
            json={
                "schema_version": "uparser.pipeline.v2",
                "request_id": "request-1",
                "items": [page_payload()],
            },
        )

        self.assertEqual(response.status_code, 503)
        self.assertIn("backend not registered", response.json()["detail"])

    async def test_batch_preserves_order_and_isolates_failure(self):
        registry = BackendRegistry()

        def infer(page):
            if page.page_id == "bad":
                raise RuntimeError("fixture failure")
            return LayoutResult(regions=[])

        registry.register("layout", RegisteredBackend(infer=infer, metadata=metadata()))
        response = await self.request(
            create_app(registry),
            "POST",
            "/v2/pipeline/layout:batch",
            json={
                "schema_version": "uparser.pipeline.v2",
                "request_id": "request-1",
                "items": [page_payload("first"), page_payload("bad"), page_payload("last")],
            },
        )

        self.assertEqual(response.status_code, 200, response.text)
        items = response.json()["items"]
        self.assertEqual([item["page_id"] for item in items], ["first", "bad", "last"])
        self.assertEqual(items[1]["error"]["code"], "inference_failed")
        self.assertEqual(items[0]["result"], {"regions": []})


if __name__ == "__main__":
    unittest.main()
