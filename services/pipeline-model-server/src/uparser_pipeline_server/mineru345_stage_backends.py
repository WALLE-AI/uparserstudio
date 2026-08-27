"""Real MinerU 3.4.5 models exposed as standalone, single-purpose stage
backends — layout / formula_detect / formula_recognize / table — for the
`pipeline_v2.rs` per-stage orchestration.

`mineru345_backend.py`'s `MinerU345PageBackend` only exposes a whole-page
`do_parse()` finalize path (Markdown in, Markdown out): it does its own
internal editing/formatting and is explicitly benchmark-reference-only —
see `PIPELINE_V2_TABLE_OCR_DEFECT_ANALYSIS.md` §0.5. Before this module,
`app.py::create_configured_app` registered the standalone `layout` /
`formula_detect` / `ocr` / `formula_recognize` / `table` endpoints with
the legacy-compatible model chain (`legacy_backends.py`) *unconditionally*,
even under `UPARSER_PIPELINE_PROFILE=mineru-3.4.5` — meaning
`pipeline_v2.rs`'s Rust orchestration could never reach the newer models
regardless of what was downloaded locally (D5 in the defect analysis).
This module is the fix: real 3.4.5 models, wrapped one stage at a time,
with all cross-stage editing/decision logic left in Rust per the "except
model inference, everything is Rust" architecture constraint (§0.5).

Every model here is constructed via `mineru.backend.pipeline.model_init`'s
own init functions (`pp_doclayout_v2_model_init`, `ocr_model_init`,
`mfr_model_init`, `table_cls_model_init`, `wired_table_model_init`,
`wireless_table_model_init`) — the exact same functions
`MineruPipelineModel.__init__` calls for the already-proven `do_parse()`
finalize path — rather than hand-resolving weight file paths here. This
guarantees identical weight resolution (via
`auto_download_and_get_model_root_path`, which honors the
`MINERU_TOOLS_CONFIG_JSON` env var pointing at the same `mineru.json`
`models-dir.pipeline` root `mineru345_backend.py` already validates)
without duplicating or guessing at that logic a second time.

Two real MinerU 3.4.5 pipeline facts drove this module's shape (both
confirmed by reading `mineru/backend/pipeline/model_init.py` and
`mineru/backend/pipeline/pipeline_magic_model.py` directly, not assumed):

1. PP-DocLayoutV2 is a *joint* layout+formula-detection model — its own
   native label set (`PP_DOCLAYOUT_V2_LABELS` in
   `mineru/model/layout/pp_doclayoutv2.py`) already includes
   `display_formula`/`inline_formula` directly. There is no standalone
   MFD (formula-detection) model in 3.4.5 the way legacy's YOLOv8-MFD is
   one. `Mineru345FormulaDetectBackend` therefore does not run a second
   model at all — it shares `Mineru345LayoutBackend`'s own
   content-hash-memoized inference result and filters it, so
   `pipeline_v2.rs`'s two concurrent dispatches
   (`layout`/`formula_detect`) don't pay for the heavy model twice.
2. The real label vocabulary emitted by PP-DocLayoutV2 (25 labels) is
   itself distinct from both the legacy vocabulary
   (`compat.py::LEGACY_LAYOUT_LABELS`) and from the pre-existing
   `category_map.rs::PIPELINE_LAYOUT_CATEGORIES` constant (which, despite
   its own doc comment, turned out to be a *third*, non-matching
   vocabulary — see the defect analysis's S3 notes; `category_map.rs`
   must be extended to accept the real `PP_DOCLAYOUT_V2_LABELS` set
   before this backend's output is fully consumable). `Mineru345LayoutBackend`
   returns the raw native label unmapped; category normalization is a
   Rust responsibility per the architecture constraint, not something
   this module pre-translates.

The table stage is deliberately, explicitly scoped down from MinerU's
real pipeline: real 3.4.5 additionally runs a table-orientation
classifier, runs *both* the wired and wireless recognition models on
every table and switches between them via a cell-count/text-count
heuristic (`UnetTableModel.predict`'s `wireless_html_code` comparison),
and binds in-table images/formulas as inline HTML tokens
(`BatchAnalyze._extract_table_inline_objects`). Faithfully porting all of
that is a substantially larger, separate effort; `Mineru345TableBackend`
here only runs the table-type classifier once and dispatches to the
matching single model — a real, working, but reduced-fidelity table
stage. This gap is intentional and tracked as an S3.1 follow-up, not
hidden.

None of this module's classes have been run against real weights in this
session — there is no working MinerU/torch environment in this sandbox
(see the defect analysis's S3 section). Every class is written the same
injectable-loader way every other backend in this package is, so it is
offline-testable against fake loaders (proving the request/response
shaping and stage-splitting logic) even though the real model classes'
exact runtime behavior can only be confirmed once run against real
weights on a real machine.
"""

from __future__ import annotations

import hashlib
import os
import sys
import threading
from pathlib import Path
from typing import Callable

import numpy as np

from .legacy_backends import _decode_page
from .mineru345_backend import pp_formula_enabled
from .recognition_backends import CurrentMineruMfrBackend, LegacyOcrBackend
from .registry import BackendRegistry, ModelRegistry, RegisteredBackend
from .schemas import (
    FormulaDetectionResult,
    LayoutResult,
    ModelMetadata,
    PageImage,
    Region,
    RecognizedTable,
    TableRecognitionInput,
    TableRecognitionResult,
)
from .table_backend import _structure_tokens, _table_ocr, table_cell_from_geometry

# Labels PP-DocLayoutV2 emits for formula regions. Both are handled by
# `Mineru345FormulaDetectBackend` — legacy's MFD backend only ever emitted
# these two labels as well (`compat.py::LEGACY_MFD_LABELS`), so
# downstream Rust code (which already accepts both, see
# `category_map.rs`'s `pipeline_category_accepts_the_legacy_backend_label_set`
# test) needs no further change to consume this backend's output.
FORMULA_LABELS = {"inline_formula", "display_formula"}


def _page_image_hash(page: PageImage) -> str:
    return hashlib.sha256(page.image.base64_data.encode("ascii")).hexdigest()


def _region_from_layout_item(page_id: str, index: int, item: dict) -> Region:
    x0, y0, x1, y1 = (float(value) for value in item["bbox"])
    confidence = item.get("score")
    return Region(
        region_id=f"{page_id}/layout-{index}",
        label=str(item["label"]),
        bbox=(x0, y0, x1, y1),
        polygon=None,
        confidence=min(1.0, max(0.0, float(confidence))) if confidence is not None else None,
        coordinate_space="render_pixels",
    )


def ensure_mineru_env(source_root: Path, config_path: Path, device: str | None) -> None:
    """Match `MinerU345PageBackend._load_parser`'s environment setup, so
    `auto_download_and_get_model_root_path` (called deep inside every
    `model_init.py` init function below) resolves against the same
    `mineru.json`/`models-dir.pipeline` root the already-proven
    `do_parse()` finalize path uses."""
    os.environ["MINERU_TOOLS_CONFIG_JSON"] = str(config_path)
    if device:
        os.environ["MINERU_DEVICE_MODE"] = device
    root = str(source_root)
    if root not in sys.path:
        sys.path.insert(0, root)


class _LayoutResultCache:
    """A small, thread-safe, insertion-order-bounded cache from page image
    content hash to raw `PPDocLayoutV2LayoutModel.predict()` output.

    Exists solely so `layout` and `formula_detect` — two independent HTTP
    endpoints `pipeline_v2.rs` dispatches concurrently for the *same*
    page image (see `run_workflow`'s `tokio::join!`) — don't each trigger
    a full model forward pass. A cache miss (e.g. the two requests race
    and neither sees the other's not-yet-inserted entry) just means one
    extra inference, never wrong output — this is a performance
    optimization, not a correctness dependency.
    """

    def __init__(self, capacity: int = 8):
        self._capacity = capacity
        self._lock = threading.Lock()
        self._entries: dict[str, list[dict]] = {}
        self._order: list[str] = []

    def get_or_compute(self, key: str, compute: Callable[[], list[dict]]) -> list[dict]:
        with self._lock:
            cached = self._entries.get(key)
            if cached is not None:
                return cached
        result = compute()
        with self._lock:
            if key not in self._entries:
                self._entries[key] = result
                self._order.append(key)
                while len(self._order) > self._capacity:
                    oldest = self._order.pop(0)
                    self._entries.pop(oldest, None)
        return result


class Mineru345LayoutBackend:
    """Wraps `PPDocLayoutV2LayoutModel` (via `model_init.pp_doclayout_v2_model_init`).
    Returns every region PP-DocLayoutV2 detects, native label unmapped
    (including `display_formula`/`inline_formula` — see this module's doc
    comment, fact 1)."""

    def __init__(
        self,
        source_root: Path,
        config_path: Path,
        device: str | None,
        loader: Callable[[Path, Path, str | None], object] | None = None,
    ):
        self.source_root = source_root
        self.config_path = config_path
        self.device = device
        self.loader = loader or self._default_loader
        self.models = ModelRegistry()
        self.inference_lock = threading.RLock()
        self.cache = _LayoutResultCache()

    @staticmethod
    def _default_loader(source_root: Path, config_path: Path, device: str | None):
        ensure_mineru_env(source_root, config_path, device)
        from mineru.backend.pipeline.model_init import pp_doclayout_v2_model_init
        from mineru.utils.enum_class import ModelPath
        from mineru.utils.models_download_utils import auto_download_and_get_model_root_path

        weight = str(
            Path(auto_download_and_get_model_root_path(ModelPath.pp_doclayout_v2))
            / ModelPath.pp_doclayout_v2
        )
        return pp_doclayout_v2_model_init(weight, device or "cpu")

    def raw_predict(self, page: PageImage) -> list[dict]:
        """Return PP-DocLayoutV2's raw per-region dict list for `page`,
        computing at most once per distinct page image content."""
        model = self.models.get_or_load(
            ("pp_doclayout_v2", str(self.config_path), self.device),
            lambda: self.loader(self.source_root, self.config_path, self.device),
        )

        def compute() -> list[dict]:
            with _decode_page(page) as image, self.inference_lock:
                return model.predict(image)

        return self.cache.get_or_compute(_page_image_hash(page), compute)

    def __call__(self, page: PageImage) -> LayoutResult:
        regions = [
            _region_from_layout_item(page.page_id, index, item)
            for index, item in enumerate(self.raw_predict(page))
        ]
        return LayoutResult(regions=regions)


class Mineru345FormulaDetectBackend:
    """Filters `Mineru345LayoutBackend`'s own (memoized) output for formula
    regions instead of running a second model — see this module's doc
    comment, fact 1: PP-DocLayoutV2 has no standalone MFD counterpart."""

    def __init__(self, layout_backend: Mineru345LayoutBackend):
        self.layout_backend = layout_backend

    def __call__(self, page: PageImage) -> FormulaDetectionResult:
        regions = [
            _region_from_layout_item(page.page_id, index, item)
            for index, item in enumerate(self.layout_backend.raw_predict(page))
            if item.get("label") in FORMULA_LABELS
        ]
        return FormulaDetectionResult(regions=regions)


def mineru345_formula_recognize_loader(source_root: Path, config_path: Path, device: str | None):
    """Build the MFR model directly — UniMERNet-small or
    PP-FormulaNet-plus-M, selected by `pp_formula_enabled()` — resolving
    its weight directory the same way `MineruPipelineModel.__init__` does.

    **Does not delegate to `model_init.py::mfr_model_init`**, even though
    that function nominally does the same class selection: it branches on
    its own module-level `MFR_MODEL` global
    (`os.getenv('MINERU_FORMULA_CH_SUPPORT', 'False')`, default
    `'False'`), which disagrees with `pp_formula_enabled()`'s default
    (`'True'`) for the *same* env var whenever it's unset. A real run
    against real weights (no env var set, so both defaults were in play)
    hit exactly this: this loader picked the PP-FormulaNet-plus-M weight
    directory (`pp_formula_enabled()` said `True`) but
    `mfr_model_init` constructed a `UnimernetModel` around it (its own
    `MFR_MODEL` global said `'False'`) — a weight/class mismatch that
    failed at load time with `"vision-encoder-decoder cannot be
    instantiated because not both encoder and decoder sub-configurations
    are passed"` (UnimernetModel expects an encoder/decoder config shape
    PP-FormulaNet-plus-M's checkpoint doesn't have). Constructing the
    class directly here, keyed on the same `pp_formula_enabled()` this
    loader already uses for weight resolution, eliminates the
    disagreement entirely instead of just aligning the two defaults
    (which would leave the same class-of-bug possible if either default
    changes again independently). Both models' underlying
    `predict(mfd_res_like, image, batch_size=...)` signatures are
    call-compatible with the existing `CurrentMineruMfrBackend` wrapper
    (`recognition_backends.py`) — confirmed by reading
    `FormulaRecognizer.predict`/`batch_predict` and `UnimernetModel.predict`
    directly."""
    ensure_mineru_env(source_root, config_path, device)
    from mineru.utils.enum_class import ModelPath
    from mineru.utils.models_download_utils import auto_download_and_get_model_root_path

    resolved_device = device or "cpu"
    if pp_formula_enabled():
        from mineru.model.mfr.pp_formulanet_plus_m.predict_formula import FormulaRecognizer

        weight_dir = str(
            Path(auto_download_and_get_model_root_path(ModelPath.pp_formulanet_plus_m))
            / ModelPath.pp_formulanet_plus_m
        )
        return FormulaRecognizer(weight_dir, resolved_device)

    from mineru.model.mfr.unimernet.Unimernet import UnimernetModel

    weight_dir = str(
        Path(auto_download_and_get_model_root_path(ModelPath.unimernet_small))
        / ModelPath.unimernet_small
    )
    return UnimernetModel(weight_dir, resolved_device)


class Mineru345TableBackend:
    """Real but explicitly reduced-fidelity 3.4.5 table stage: classify
    wired vs. wireless once (`PaddleTableClsModel`) and dispatch to the
    matching single recognition model, both built the same way
    `MineruPipelineModel.__init__` builds them. Does not replicate real
    MinerU's table-orientation classification, dual wired/wireless run
    with a cell/text-count switch heuristic, or in-table image/formula
    inline binding — see this module's doc comment for why those are a
    tracked follow-up (S3.1), not silently dropped."""

    def __init__(
        self,
        source_root: Path,
        config_path: Path,
        device: str | None,
        lang: str = "ch",
        loader: Callable[[Path, Path, str | None, str], tuple] | None = None,
    ):
        self.source_root = source_root
        self.config_path = config_path
        self.device = device
        self.lang = lang
        self.loader = loader or self._default_loader
        self.models = ModelRegistry()
        self.inference_lock = threading.RLock()

    @staticmethod
    def _default_loader(source_root: Path, config_path: Path, device: str | None, lang: str):
        ensure_mineru_env(source_root, config_path, device)
        from mineru.backend.pipeline.model_init import (
            table_cls_model_init,
            wired_table_model_init,
            wireless_table_model_init,
        )

        return (
            table_cls_model_init(),
            wireless_table_model_init(lang),
            wired_table_model_init(lang),
        )

    def _models(self):
        return self.models.get_or_load(
            (str(self.config_path), self.device, self.lang),
            lambda: self.loader(self.source_root, self.config_path, self.device, self.lang),
        )

    def __call__(self, item: TableRecognitionInput) -> TableRecognitionResult:
        if not item.table_regions:
            return TableRecognitionResult(tables=[])
        table_cls_model, wireless_model, wired_model = self._models()
        with _decode_page(item.page) as image:
            page_rgb = np.asarray(image, dtype=np.uint8)

        tables: list[RecognizedTable] = []
        page_height, page_width = page_rgb.shape[:2]
        for region in item.table_regions:
            x0, y0, x1, y1 = region.bbox
            left = max(0, min(page_width, int(x0)))
            top = max(0, min(page_height, int(y0)))
            right = max(0, min(page_width, int(np.ceil(x1))))
            bottom = max(0, min(page_height, int(np.ceil(y1))))
            if right <= left or bottom <= top:
                raise ValueError(f"table has no pixels after clipping: {region.region_id}")
            crop = page_rgb[top:bottom, left:right]
            ocr_result, selected_spans = _table_ocr(
                item.ocr_spans, item.formula_spans, (left, top, right, bottom)
            )
            with self.inference_lock:
                # `PaddleTableClsModel.predict` returns MinerU's own
                # `AtomicModel.{Wired,Wireless}Table` string constants
                # ("wired_table"/"wireless_table"), not this project's
                # normalized "wired"/"wireless" — confirmed by reading
                # `paddle_table_cls.py::predict` directly.
                raw_label, _confidence = table_cls_model.predict(crop)

            # Neither model's `predict()` returns the `PaddleTableOutput`-
            # shaped object the legacy backend
            # (`table_backend.py::CurrentMineruSlanetBackend`) consumes —
            # confirmed by reading both implementations directly, not
            # assumed from the legacy shape:
            #   - `PaddleTableModel.predict` (wireless) returns a plain
            #     4-tuple `(html, cell_bboxes, logic_points, elapse)`.
            #   - `UnetTableModel.predict` (wired) with
            #     `return_metadata=True` returns a dict whose
            #     `wired_cell_bboxes`/`wired_logic_points` belong to the
            #     wired model's own structure regardless of which HTML
            #     `dict["html"]` ends up selected — an inconsistency in
            #     MinerU's own `UnetTableModel.predict`, not introduced
            #     here. Passing `wireless_html_code=""` deterministically
            #     keeps its internal switch-heuristic from overriding our
            #     classifier's decision (an empty string never satisfies
            #     any of its cell/text-count switch conditions).
            if raw_label == "wired_table":
                classifier_label = "wired"
                metadata = wired_model.predict(
                    crop, ocr_result, wireless_html_code="", return_metadata=True
                )
                markup = metadata.get("html") or ""
                cell_boxes = metadata.get("wired_cell_bboxes")
                logic_points = metadata.get("wired_logic_points")
            else:
                classifier_label = "wireless"
                markup, cell_boxes, logic_points, _elapse = wireless_model.predict(
                    crop, ocr_result
                )
                markup = markup or ""
            cell_boxes = cell_boxes if cell_boxes is not None else []
            logic_points = logic_points if logic_points is not None else []

            cells = [
                table_cell_from_geometry(
                    region.region_id, index, box, logic, left, top, selected_spans
                )
                for index, (box, logic) in enumerate(zip(cell_boxes, logic_points))
            ]
            tables.append(
                RecognizedTable(
                    region_id=region.region_id,
                    html=markup,
                    structure_tokens=_structure_tokens(markup),
                    cells=cells,
                    classifier_label=classifier_label,
                    rotation_degrees=0,
                    confidence=region.confidence,
                )
            )
        return TableRecognitionResult(tables=tables)


def mineru345_ocr_loader_factory(
    source_root: Path, config_path: Path
) -> Callable[[str, Path, str | None], object]:
    """Build a `LegacyOcrBackend`-compatible loader
    (`(language, models_root, device) -> model`) that constructs the OCR
    engine via `model_init.py::ocr_model_init` instead of the legacy
    `magic_pdf` loader path — the real PytorchPaddleOCR class 3.4.5 uses
    (`mineru.model.ocr.pytorch_paddle.PytorchPaddleOCR`, real
    `ch_PP-OCRv6` weights), resolved the same way
    `MineruHybridModel`/`MineruPipelineModel` resolve it. `models_root`
    is accepted only to match `LegacyOcrBackend`'s loader signature — it
    is unused here since `ocr_model_init` resolves its own weights via
    `MINERU_TOOLS_CONFIG_JSON`, already set by `ensure_mineru_env`."""

    def loader(language: str, _models_root: Path, device: str | None):
        ensure_mineru_env(source_root, config_path, device)
        from mineru.backend.pipeline.model_init import ocr_model_init

        return ocr_model_init(lang=language)

    return loader


def register_mineru345_stage_backends(
    registry: BackendRegistry,
    source_root: Path,
    config_path: Path,
    device: str | None = None,
) -> None:
    """Register real MinerU 3.4.5 models against the standalone
    `layout`/`formula_detect`/`ocr`/`formula_recognize`/`table` stage
    endpoints `pipeline_v2.rs` actually dispatches to — as opposed to
    `register_mineru345_page_backend` (`mineru345_backend.py`), which only
    registers the whole-page/whole-document finalize endpoints
    (`pages_analyze`/`documents_analyze`, benchmark-reference-only, see
    §0.5 of the defect analysis). Call this *instead of*
    `register_pipeline_backends` when
    `UPARSER_PIPELINE_PROFILE=mineru-3.4.5` — calling both would try to
    register the same five stage names twice and raise
    (`BackendRegistry.register` rejects duplicate stages)."""
    layout_backend = Mineru345LayoutBackend(source_root, config_path, device)
    revision = "mineru-3.4.5-pp-doclayout-v2"

    registry.register(
        "layout",
        RegisteredBackend(
            infer=layout_backend,
            metadata=ModelMetadata(
                name="pp_doclayout_v2",
                revision=revision,
                weight_sha256=None,
                runtime="mineru-3.4.5-source",
            ),
        ),
    )
    registry.register(
        "formula_detect",
        RegisteredBackend(
            infer=Mineru345FormulaDetectBackend(layout_backend),
            metadata=ModelMetadata(
                name="pp_doclayout_v2_formula_regions",
                revision=revision,
                weight_sha256=None,
                runtime="mineru-3.4.5-source (derived from the layout stage, no separate model)",
            ),
        ),
    )
    registry.register(
        "ocr",
        RegisteredBackend(
            infer=LegacyOcrBackend(
                models_root=source_root,
                device=device or "cpu",
                loader=mineru345_ocr_loader_factory(source_root, config_path),
            ),
            metadata=ModelMetadata(
                name="paddleocr_torch_pp_ocrv6",
                revision=revision,
                weight_sha256=None,
                runtime="mineru-3.4.5-source",
            ),
        ),
    )
    formula_model_name = "pp_formulanet_plus_m" if pp_formula_enabled() else "unimernet_small"
    registry.register(
        "formula_recognize",
        RegisteredBackend(
            infer=CurrentMineruMfrBackend(
                weight_dir=source_root,  # unused: loader resolves its own weight_dir
                reference_root=source_root,
                device=device or "cpu",
                loader=lambda _weight_dir, _reference_root, dev: mineru345_formula_recognize_loader(
                    source_root, config_path, dev
                ),
            ),
            metadata=ModelMetadata(
                name=formula_model_name,
                revision=revision,
                weight_sha256=None,
                runtime="mineru-3.4.5-source",
            ),
        ),
    )
    registry.register(
        "table",
        RegisteredBackend(
            infer=Mineru345TableBackend(source_root, config_path, device),
            metadata=ModelMetadata(
                name="pp_table_cls+slanet_plus+unet_structure",
                revision=revision,
                weight_sha256=None,
                runtime="mineru-3.4.5-source",
            ),
        ),
    )
