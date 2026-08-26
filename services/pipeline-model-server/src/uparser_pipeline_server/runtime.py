"""Batch execution with deterministic ordering and item-level isolation."""

from __future__ import annotations

import logging
from typing import Callable, TypeVar

from pydantic import BaseModel

from .schemas import BatchItemResult, StageError, StageWarning


InputT = TypeVar("InputT", bound=BaseModel)
ResultT = TypeVar("ResultT", bound=BaseModel)


def page_id_of(item: BaseModel) -> str:
    if hasattr(item, "document"):
        return str(item.document.document_id)
    page = getattr(item, "page", item)
    return str(getattr(page, "page_id"))


def run_isolated_batch(
    items: list[InputT],
    infer: Callable[[InputT], ResultT],
) -> list[BatchItemResult[ResultT]]:
    results = []
    for item in items:
        page_id = page_id_of(item)
        try:
            result = infer(item)
            if not isinstance(result, BaseModel):
                raise TypeError("backend must return a validated Pydantic result")
            results.append(BatchItemResult(page_id=page_id, result=result))
        except Exception as exc:
            results.append(
                BatchItemResult(
                    page_id=page_id,
                    error=StageError(
                        code="inference_failed",
                        message=str(exc),
                        retryable=False,
                    ),
                )
            )
    return results


def run_backend_batch(items, backend) -> list[BatchItemResult]:
    """Use a native batch path when available, with isolated fallback on failure."""
    if backend.infer_batch is None:
        return run_isolated_batch(items, backend.infer)
    try:
        values = backend.infer_batch(items)
        if len(values) != len(items):
            raise ValueError(
                f"batch backend returned {len(values)} results for {len(items)} inputs"
            )
        results = []
        for item, value in zip(items, values):
            if not isinstance(value, BaseModel):
                raise TypeError("batch backend must return validated Pydantic results")
            results.append(BatchItemResult(page_id=page_id_of(item), result=value))
        return results
    except Exception as batch_error:
        # A batch-level failure must not violate the per-item isolation contract.
        logging.exception("native backend batch failed; retrying items independently")
        results = run_isolated_batch(items, backend.infer)
        warning = StageWarning(
            code="batch_fallback",
            message=f"native batch failed; retried items independently: {batch_error}",
        )
        for result in results:
            result.warnings.append(warning)
        return results
