"""Thread-safe model/backend registry modeled after current MinerU singletons."""

from __future__ import annotations

from dataclasses import dataclass
from threading import RLock
from typing import Callable, Generic, Hashable, TypeVar

from .schemas import ModelMetadata


ModelT = TypeVar("ModelT")


class ModelRegistry(Generic[ModelT]):
    def __init__(self):
        self._models: dict[Hashable, ModelT] = {}
        self._lock = RLock()

    def get_or_load(self, key: Hashable, loader: Callable[[], ModelT]) -> ModelT:
        with self._lock:
            if key not in self._models:
                self._models[key] = loader()
            return self._models[key]

    def snapshot(self) -> dict[Hashable, ModelT]:
        with self._lock:
            return dict(self._models)

    def clear(self, closer: Callable[[ModelT], None] | None = None) -> None:
        with self._lock:
            values = list(self._models.values())
            self._models.clear()
        if closer is not None:
            for value in values:
                closer(value)


@dataclass(frozen=True)
class RegisteredBackend:
    infer: Callable[[object], object]
    metadata: ModelMetadata
    infer_batch: Callable[[list[object]], list[object]] | None = None


class BackendRegistry:
    def __init__(self):
        self._backends: dict[str, RegisteredBackend] = {}
        self._lock = RLock()

    def register(self, stage: str, backend: RegisteredBackend) -> None:
        with self._lock:
            if stage in self._backends:
                raise ValueError(f"backend already registered for stage {stage}")
            self._backends[stage] = backend

    def get(self, stage: str) -> RegisteredBackend | None:
        with self._lock:
            return self._backends.get(stage)

    def metadata(self) -> dict[str, ModelMetadata]:
        with self._lock:
            return {stage: backend.metadata for stage, backend in self._backends.items()}
