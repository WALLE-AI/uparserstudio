"""Batch execution with deterministic ordering and item-level isolation."""

from __future__ import annotations

from typing import Callable, TypeVar

from pydantic import BaseModel

from .schemas import BatchItemResult, StageError


InputT = TypeVar("InputT", bound=BaseModel)
ResultT = TypeVar("ResultT", bound=BaseModel)


def page_id_of(item: BaseModel) -> str:
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
