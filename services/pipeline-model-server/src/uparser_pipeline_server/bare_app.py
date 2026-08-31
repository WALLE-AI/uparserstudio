"""FastAPI entry point exposing only bare tensor model inference."""

from __future__ import annotations

import asyncio
import os
import queue
import threading
import time
from pathlib import Path
from typing import Any

import numpy as np
import uvicorn
from fastapi import FastAPI, HTTPException, Request, Response

from .bare_models import BareModelRegistry, create_mineru345_bare_registry
from .tensor_wire import TensorBundle, decode, encode


CONTENT_TYPE = "application/vnd.uparser.tensor"


def _batching_allowed(model_name: str) -> bool:
    accuracy_sensitive = {"pp_doclayout_v2", "pp_formulanet_plus_m"}
    forced = {
        name.strip()
        for name in os.getenv("UPARSER_BARE_FORCE_BATCH_MODELS", "").split(",")
        if name.strip()
    }
    return model_name not in accuracy_sensitive or model_name in forced


class _InferenceWorker:
    """Serialize one model while allowing independent models to overlap."""

    MAX_BATCH_REQUESTS = 16
    MAX_BATCH_ITEMS = 16
    BATCH_WAIT_SECONDS = 0.003

    def __init__(self, model: Any, *, allow_batching: bool):
        self.model = model
        self.allow_batching = allow_batching
        self.requests: queue.Queue = queue.Queue()
        self.thread = threading.Thread(
            target=self._run,
            name=f"bare-model-{model.name}",
            daemon=True,
        )
        self.thread.start()

    async def infer(self, inputs: TensorBundle) -> TensorBundle:
        response: queue.Queue = queue.Queue(maxsize=1)
        self.requests.put((inputs, response))
        while True:
            try:
                result, error = response.get_nowait()
            except queue.Empty:
                await asyncio.sleep(0.001)
                continue
            if error is not None:
                raise error
            return result

    @staticmethod
    def _compatible(left: TensorBundle, right: TensorBundle) -> bool:
        if left.tensors.keys() != right.tensors.keys():
            return False
        return all(
            left.tensors[name].ndim > 0
            and right.tensors[name].ndim > 0
            and left.tensors[name].dtype == right.tensors[name].dtype
            and left.tensors[name].shape[1:] == right.tensors[name].shape[1:]
            for name in left.tensors
        )

    @staticmethod
    def _batch_size(bundle: TensorBundle) -> int:
        return next(iter(bundle.tensors.values())).shape[0]

    @staticmethod
    def _merge(requests: list[TensorBundle]) -> tuple[TensorBundle, list[int]]:
        first = requests[0]
        sizes = [_InferenceWorker._batch_size(request) for request in requests]
        for request, size in zip(requests, sizes):
            if any(tensor.shape[0] != size for tensor in request.tensors.values()):
                raise ValueError("all tensors in one request must share the batch dimension")
        return (
            TensorBundle(
                tensors={
                    name: np.concatenate(
                        [request.tensors[name] for request in requests], axis=0
                    )
                    for name in first.tensors
                },
                metadata=first.metadata.copy(),
            ),
            sizes,
        )

    @staticmethod
    def _split(outputs: TensorBundle, sizes: list[int]) -> list[TensorBundle]:
        total = sum(sizes)
        if any(tensor.ndim == 0 or tensor.shape[0] != total for tensor in outputs.tensors.values()):
            raise ValueError("model output batch dimension does not match the merged input")
        offsets = [0]
        for size in sizes:
            offsets.append(offsets[-1] + size)
        return [
            TensorBundle(
                tensors={
                    name: tensor[offsets[index] : offsets[index + 1]]
                    for name, tensor in outputs.tensors.items()
                },
                metadata=outputs.metadata.copy(),
            )
            for index in range(len(sizes))
        ]

    def _run(self) -> None:
        deferred = []
        while True:
            first = deferred.pop(0) if deferred else self.requests.get()
            batch = [first]
            batch_items = self._batch_size(first[0])
            batch_item_limit = max(self.MAX_BATCH_ITEMS, batch_items)
            if not self.allow_batching:
                batch_item_limit = batch_items
            deadline = time.monotonic() + self.BATCH_WAIT_SECONDS
            while self.allow_batching and len(batch) < self.MAX_BATCH_REQUESTS:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                try:
                    item = self.requests.get(timeout=remaining)
                except queue.Empty:
                    break
                item_size = self._batch_size(item[0])
                if (
                    self._compatible(first[0], item[0])
                    and batch_items + item_size <= batch_item_limit
                ):
                    batch.append(item)
                    batch_items += item_size
                else:
                    deferred.append(item)
            try:
                merged, sizes = self._merge([inputs for inputs, _ in batch])
                result = self.model.infer(merged)
                results = self._split(result, sizes)
            except Exception as error:
                for _, response in batch:
                    response.put((None, error))
            else:
                for (_, response), output in zip(batch, results):
                    response.put((output, None))


def create_app(registry: BareModelRegistry) -> FastAPI:
    app = FastAPI(
        title="uparser Bare Model Server",
        version="0.1.0",
        description="Prepared tensors in; raw model tensors out",
    )
    workers: dict[str, _InferenceWorker] = {}

    @app.get("/health")
    async def health():
        return {"status": "ready", "models": sorted(registry.metadata())}

    @app.get("/v1/models")
    async def models():
        return {"schema_version": 1, "models": registry.metadata()}

    @app.post("/v1/models/{model_name}:infer")
    async def infer(model_name: str, request: Request):
        model = registry.get(model_name)
        if model is None:
            raise HTTPException(status_code=404, detail=f"model not registered: {model_name}")
        if request.headers.get("content-type", "").split(";", 1)[0] != CONTENT_TYPE:
            raise HTTPException(status_code=415, detail=f"content-type must be {CONTENT_TYPE}")
        try:
            inputs = decode(await request.body())
            worker = workers.get(model_name)
            if worker is None:
                # Layout thresholds and autoregressive formula decoding can cross
                # decision boundaries when CUDA selects a batched kernel. Keep
                # those accuracy-sensitive models bit-stable; OCR and table
                # forwards remain safe to microbatch.
                worker = _InferenceWorker(model, allow_batching=_batching_allowed(model_name))
                workers[model_name] = worker
            outputs = await worker.infer(inputs)
            body = encode(outputs)
        except ValueError as error:
            raise HTTPException(status_code=400, detail=str(error)) from error
        return Response(content=body, media_type=CONTENT_TYPE)

    return app


def create_configured_app() -> FastAPI:
    source_root = Path(os.getenv("UPARSER_MINERU_ROOT", "opensource/MinerU")).resolve()
    config_path = Path(
        os.getenv("UPARSER_MINERU_CONFIG", "/home/dataset1/gaojing/mineru.json")
    ).resolve()
    device = os.getenv("UPARSER_PIPELINE_DEVICE", "cpu")
    return create_app(create_mineru345_bare_registry(source_root, config_path, device))


def main() -> None:
    uvicorn.run(
        "uparser_pipeline_server.bare_app:create_configured_app",
        factory=True,
        host=os.getenv("UPARSER_PIPELINE_HOST", "127.0.0.1"),
        port=int(os.getenv("UPARSER_PIPELINE_PORT", "9001")),
    )


if __name__ == "__main__":
    main()
