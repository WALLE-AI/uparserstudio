"""FastAPI entry point for the pipeline V2 model service."""

from __future__ import annotations

import os
from pathlib import Path

import uvicorn
from fastapi import FastAPI, HTTPException

from .registry import BackendRegistry
from .runtime import run_backend_batch
from .schemas import (
    BatchResponse,
    DocumentAnalyzeBatchRequest,
    DocumentAnalyzeBatchResponse,
    DocumentAnalyzeResult,
    FormulaDetectionBatchRequest,
    FormulaDetectionBatchResponse,
    FormulaDetectionResult,
    FormulaRecognitionBatchRequest,
    FormulaRecognitionBatchResponse,
    FormulaRecognitionResult,
    LayoutBatchRequest,
    LayoutBatchResponse,
    LayoutResult,
    OcrBatchRequest,
    OcrBatchResponse,
    OcrResult,
    PageAnalyzeBatchRequest,
    PageAnalyzeBatchResponse,
    PageAnalyzeResult,
    SCHEMA_VERSION,
    TableRecognitionBatchRequest,
    TableRecognitionBatchResponse,
    TableRecognitionResult,
)


REQUIRED_STAGES = {"layout", "formula_detect", "ocr", "formula_recognize", "table", "pages_analyze"}


def create_app(registry: BackendRegistry | None = None) -> FastAPI:
    registry = registry or BackendRegistry()
    app = FastAPI(
        title="uparser Pipeline Model Server",
        version="0.1.0",
        description="MinerU-current compatible pipeline V2 inference service",
    )
    app.state.backends = registry

    @app.get("/health")
    async def health():
        registered = set(registry.metadata())
        missing = sorted(REQUIRED_STAGES - registered)
        return {
            "status": "ready" if not missing else "not_ready",
            "schema_version": SCHEMA_VERSION,
            "registered_stages": sorted(registered),
            "missing_stages": missing,
        }

    @app.get("/v2/models")
    async def models():
        return {
            "schema_version": SCHEMA_VERSION,
            "models": {
                stage: metadata.model_dump(mode="json")
                for stage, metadata in registry.metadata().items()
            },
        }

    async def execute(stage, request, response_type, result_type):
        backend = registry.get(stage)
        if backend is None:
            raise HTTPException(status_code=503, detail=f"backend not registered: {stage}")
        items = run_backend_batch(request.items, backend)
        response = BatchResponse[result_type](
            request_id=request.request_id,
            model=backend.metadata,
            items=items,
        )
        return response_type.model_validate(response.model_dump(mode="python"))

    @app.post("/v2/pipeline/layout:batch", response_model=LayoutBatchResponse)
    async def layout(request: LayoutBatchRequest):
        return await execute("layout", request, LayoutBatchResponse, LayoutResult)

    @app.post("/v2/pipeline/mfd:batch", response_model=FormulaDetectionBatchResponse)
    async def formula_detect(request: FormulaDetectionBatchRequest):
        return await execute(
            "formula_detect", request, FormulaDetectionBatchResponse, FormulaDetectionResult
        )

    @app.post("/v2/pipeline/ocr:batch", response_model=OcrBatchResponse)
    async def ocr(request: OcrBatchRequest):
        return await execute("ocr", request, OcrBatchResponse, OcrResult)

    @app.post("/v2/pipeline/mfr:batch", response_model=FormulaRecognitionBatchResponse)
    async def formula_recognize(request: FormulaRecognitionBatchRequest):
        return await execute(
            "formula_recognize",
            request,
            FormulaRecognitionBatchResponse,
            FormulaRecognitionResult,
        )

    @app.post("/v2/pipeline/table:batch", response_model=TableRecognitionBatchResponse)
    async def table(request: TableRecognitionBatchRequest):
        return await execute("table", request, TableRecognitionBatchResponse, TableRecognitionResult)

    @app.post("/v2/pipeline/pages:analyze", response_model=PageAnalyzeBatchResponse)
    async def pages_analyze(request: PageAnalyzeBatchRequest):
        return await execute("pages_analyze", request, PageAnalyzeBatchResponse, PageAnalyzeResult)

    @app.post("/v2/pipeline/documents:analyze", response_model=DocumentAnalyzeBatchResponse)
    async def documents_analyze(request: DocumentAnalyzeBatchRequest):
        return await execute(
            "documents_analyze",
            request,
            DocumentAnalyzeBatchResponse,
            DocumentAnalyzeResult,
        )

    return app


def create_configured_app() -> FastAPI:
    from .legacy_backends import register_pipeline_backends
    from .mineru345_backend import register_mineru345_page_backend

    manifest = Path(
        os.getenv("UPARSER_PIPELINE_MANIFEST", "pipeline/model-manifest.json")
    ).resolve()
    device = os.getenv("UPARSER_PIPELINE_DEVICE")
    profile = os.getenv("UPARSER_PIPELINE_PROFILE", "mineru-3.4.5").strip().lower()
    registry = BackendRegistry()
    if profile not in {"mineru-3.4.5", "legacy"}:
        raise ValueError(f"unsupported UPARSER_PIPELINE_PROFILE: {profile}")
    register_pipeline_backends(
        registry,
        manifest,
        device,
        include_page_analyzer=profile == "legacy",
    )
    if profile == "mineru-3.4.5":
        source_root = Path(
            os.getenv("UPARSER_MINERU_ROOT", "opensource/MinerU")
        ).resolve()
        config_path = Path(
            os.getenv("UPARSER_MINERU_CONFIG", "/home/dataset1/gaojing/mineru.json")
        ).resolve()
        register_mineru345_page_backend(registry, source_root, config_path, device)
    return create_app(registry)


def main() -> None:
    uvicorn.run(
        "uparser_pipeline_server.app:create_configured_app",
        factory=True,
        host=os.getenv("UPARSER_PIPELINE_HOST", "127.0.0.1"),
        port=int(os.getenv("UPARSER_PIPELINE_PORT", "9001")),
    )


if __name__ == "__main__":
    main()
