"""FastAPI entry point exposing only bare tensor model inference."""

from __future__ import annotations

import os
from pathlib import Path

import uvicorn
from fastapi import FastAPI, HTTPException, Request, Response

from .bare_models import BareModelRegistry, create_mineru345_bare_registry
from .tensor_wire import decode, encode


CONTENT_TYPE = "application/vnd.uparser.tensor"


def create_app(registry: BareModelRegistry) -> FastAPI:
    app = FastAPI(
        title="uparser Bare Model Server",
        version="0.1.0",
        description="Prepared tensors in; raw model tensors out",
    )

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
            outputs = model.infer(inputs)
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
