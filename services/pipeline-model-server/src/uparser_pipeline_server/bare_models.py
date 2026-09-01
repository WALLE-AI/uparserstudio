"""Bare model runners: prepared tensors in, raw model tensors out."""

from __future__ import annotations

import os
import sys
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import numpy as np

from .tensor_wire import TensorBundle


def _writable_array(value: np.ndarray) -> np.ndarray:
    array = np.asarray(value)
    if array.flags.writeable and array.flags.c_contiguous:
        return array
    return np.array(array, copy=True, order="C")


def _raw_tensors(value, prefix: str = "output") -> dict[str, np.ndarray]:
    """Flatten a model return value without interpreting any tensor."""
    import torch

    if isinstance(value, torch.Tensor):
        value = value.detach().to("cpu")
        if value.is_floating_point():
            value = value.to(dtype=torch.float32)
        elif value.dtype != torch.int64:
            value = value.to(dtype=torch.int64)
        return {prefix: value.numpy()}
    if isinstance(value, dict):
        tensors: dict[str, np.ndarray] = {}
        for name, item in value.items():
            tensors.update(_raw_tensors(item, str(name)))
        return tensors
    if isinstance(value, (list, tuple)):
        tensors = {}
        for index, item in enumerate(value):
            tensors.update(_raw_tensors(item, f"{prefix}_{index}"))
        return tensors
    if value is None or isinstance(value, (bool, int, float, str)):
        return {}
    raise ValueError(f"model returned unsupported non-tensor value: {type(value).__name__}")


@dataclass(frozen=True)
class BareModel:
    name: str
    revision: str
    input_schema: dict
    output_schema: dict
    infer: Callable[[TensorBundle], TensorBundle]


class BareModelRegistry:
    def __init__(self):
        self._models: dict[str, BareModel] = {}

    def register(self, model: BareModel) -> None:
        if model.name in self._models:
            raise ValueError(f"bare model already registered: {model.name}")
        self._models[model.name] = model

    def get(self, name: str) -> BareModel | None:
        return self._models.get(name)

    def metadata(self) -> dict[str, dict]:
        return {
            name: {
                "name": model.name,
                "revision": model.revision,
                "input_schema": model.input_schema,
                "output_schema": model.output_schema,
            }
            for name, model in self._models.items()
        }


class LayoutForward:
    """PP-DocLayoutV2 forward only; no image or detection postprocessing."""

    def __init__(self, source_root: Path, config_path: Path, device: str):
        os.environ["MINERU_TOOLS_CONFIG_JSON"] = str(config_path)
        if str(source_root) not in sys.path:
            sys.path.insert(0, str(source_root))
        from mineru.backend.pipeline.model_init import pp_doclayout_v2_model_init
        from mineru.utils.enum_class import ModelPath
        from mineru.utils.models_download_utils import auto_download_and_get_model_root_path

        weight = str(
            Path(auto_download_and_get_model_root_path(ModelPath.pp_doclayout_v2))
            / ModelPath.pp_doclayout_v2
        )
        wrapper = pp_doclayout_v2_model_init(weight, device)
        self.model = wrapper.model
        self.device = device
        self.lock = threading.RLock()

    def __call__(self, bundle: TensorBundle) -> TensorBundle:
        import torch

        if set(bundle.tensors) != {"pixel_values"}:
            raise ValueError("layout requires exactly one tensor named pixel_values")
        pixels = bundle.tensors["pixel_values"]
        if pixels.dtype != np.dtype("<f4") or pixels.ndim != 4 or pixels.shape[1] != 3:
            raise ValueError("pixel_values must be f32 [batch,3,height,width]")
        # Tensor-wire decoding yields a read-only view over the request body.
        # PyTorch requires writable NumPy storage even for inference-only tensors.
        tensor = torch.from_numpy(_writable_array(pixels)).to(self.device)
        with self.lock, torch.no_grad():
            outputs = self.model(pixel_values=tensor)
        raw = {
            "logits": outputs.logits.detach().to("cpu", dtype=torch.float32).numpy(),
            "pred_boxes": outputs.pred_boxes.detach().to("cpu", dtype=torch.float32).numpy(),
            "order_logits": outputs.order_logits.detach().to("cpu", dtype=torch.float32).numpy(),
        }
        return TensorBundle(
            tensors=raw,
            metadata={
                "model": "pp_doclayout_v2",
                "revision": bundle.metadata.get("revision", "mineru-3.4.5"),
            },
        )


class OcrForward:
    """PP-OCRv6 detector/recognizer network forwards without OCR logic."""

    def __init__(self, source_root: Path, config_path: Path, device: str):
        os.environ["MINERU_TOOLS_CONFIG_JSON"] = str(config_path)
        os.environ["MINERU_DEVICE_MODE"] = device
        if str(source_root) not in sys.path:
            sys.path.insert(0, str(source_root))
        from mineru.backend.pipeline.model_init import ocr_model_init

        wrapper = ocr_model_init(lang="ch")
        self.detector = wrapper.text_detector.net
        self.recognizer = wrapper.text_recognizer.net
        self.device = device
        self.lock = threading.RLock()

    def _forward(self, bundle: TensorBundle, model, model_name: str) -> TensorBundle:
        import torch

        if set(bundle.tensors) != {"pixel_values"}:
            raise ValueError(f"{model_name} requires exactly pixel_values")
        pixels = bundle.tensors["pixel_values"]
        if pixels.dtype != np.dtype("<f4") or pixels.ndim != 4 or pixels.shape[1] != 3:
            raise ValueError("pixel_values must be f32 [batch,3,height,width]")
        dtype = next(model.parameters()).dtype
        tensor = torch.from_numpy(_writable_array(pixels)).to(self.device, dtype=dtype)
        with self.lock, torch.inference_mode():
            outputs = model(tensor)
        return TensorBundle(
            tensors=_raw_tensors(outputs),
            metadata={"model": model_name, "revision": "mineru-3.4.5"},
        )

    def detect(self, bundle: TensorBundle) -> TensorBundle:
        return self._forward(bundle, self.detector, "pp_ocrv6_det")

    def recognize(self, bundle: TensorBundle) -> TensorBundle:
        return self._forward(bundle, self.recognizer, "pp_ocrv6_rec")


class FormulaForward:
    """PP-FormulaNet network forward; Rust owns crops and token decoding."""

    def __init__(self, source_root: Path, config_path: Path, device: str):
        os.environ["MINERU_TOOLS_CONFIG_JSON"] = str(config_path)
        if str(source_root) not in sys.path:
            sys.path.insert(0, str(source_root))
        from mineru.model.mfr.pp_formulanet_plus_m.predict_formula import FormulaRecognizer
        from mineru.utils.enum_class import ModelPath
        from mineru.utils.models_download_utils import auto_download_and_get_model_root_path

        weight = str(
            Path(auto_download_and_get_model_root_path(ModelPath.pp_formulanet_plus_m))
            / ModelPath.pp_formulanet_plus_m
        )
        wrapper = FormulaRecognizer(weight, device)
        self.model = wrapper.net
        self.device = device
        self.lock = threading.RLock()

    def __call__(self, bundle: TensorBundle) -> TensorBundle:
        import torch

        if set(bundle.tensors) != {"pixel_values"}:
            raise ValueError("formula recognition requires exactly pixel_values")
        pixels = bundle.tensors["pixel_values"]
        if pixels.dtype != np.dtype("<f4") or pixels.ndim != 4 or pixels.shape[1] != 1:
            raise ValueError("pixel_values must be f32 [batch,1,height,width]")
        tensor = torch.from_numpy(_writable_array(pixels)).to(self.device)
        with self.lock, torch.inference_mode():
            outputs = self.model(tensor)
        return TensorBundle(
            tensors=_raw_tensors(outputs, "token_ids"),
            metadata={"model": "pp_formulanet_plus_m", "revision": "mineru-3.4.5"},
        )


class OnnxForward:
    """One ONNX session.run call with no preprocessing or postprocessing."""

    def __init__(self, model_path: Path, input_name: str, output_names: list[str]):
        import onnxruntime

        session_options = onnxruntime.SessionOptions()
        # The wired-table graph declares several scalar outputs as `{}` while
        # returning `{1}`. ONNX Runtime otherwise emits the same benign shape
        # warning thousands of times during a benchmark.
        session_options.log_severity_level = 3
        self.session = onnxruntime.InferenceSession(
            str(model_path), sess_options=session_options
        )
        self.input_name = input_name
        self.output_names = output_names
        self.lock = threading.RLock()

    def __call__(self, bundle: TensorBundle) -> TensorBundle:
        if set(bundle.tensors) != {self.input_name}:
            raise ValueError(f"ONNX model requires exactly tensor {self.input_name}")
        value = np.asarray(bundle.tensors[self.input_name])
        if value.dtype != np.dtype("<f4"):
            raise ValueError(f"{self.input_name} must be f32")
        with self.lock:
            outputs = self.session.run(None, {self.input_name: _writable_array(value)})
        if len(outputs) != len(self.output_names):
            raise ValueError("ONNX output count differs from the registered tensor schema")
        return TensorBundle(
            tensors=dict(zip(self.output_names, outputs)),
            metadata={"revision": "mineru-3.4.5"},
        )


def create_mineru345_bare_registry(
    source_root: Path, config_path: Path, device: str
) -> BareModelRegistry:
    registry = BareModelRegistry()
    layout_loader: LayoutForward | None = None
    lock = threading.RLock()

    def layout_infer(bundle: TensorBundle) -> TensorBundle:
        nonlocal layout_loader
        with lock:
            if layout_loader is None:
                layout_loader = LayoutForward(source_root, config_path, device)
        return layout_loader(bundle)

    registry.register(
        BareModel(
            name="pp_doclayout_v2",
            revision="mineru-3.4.5",
            input_schema={"pixel_values": {"dtype": "f32", "shape": ["batch", 3, 800, 800]}},
            output_schema={
                "logits": {"dtype": "f32", "shape": ["batch", "queries", 25]},
                "pred_boxes": {"dtype": "f32", "shape": ["batch", "queries", 4]},
                "order_logits": {
                    "dtype": "f32",
                    "shape": ["batch", "queries", "queries"],
                },
            },
            infer=layout_infer,
        )
    )
    ocr_loader: OcrForward | None = None

    def get_ocr() -> OcrForward:
        nonlocal ocr_loader
        with lock:
            if ocr_loader is None:
                ocr_loader = OcrForward(source_root, config_path, device)
        return ocr_loader

    registry.register(
        BareModel(
            name="pp_ocrv6_det",
            revision="mineru-3.4.5",
            input_schema={"pixel_values": {"dtype": "f32", "shape": ["batch", 3, "height", "width"]}},
            output_schema={"maps": {"dtype": "f32", "shape": ["batch", "channels", "height", "width"]}},
            infer=lambda bundle: get_ocr().detect(bundle),
        )
    )
    registry.register(
        BareModel(
            name="pp_ocrv6_rec",
            revision="mineru-3.4.5",
            input_schema={"pixel_values": {"dtype": "f32", "shape": ["batch", 3, 48, "width"]}},
            output_schema={
                "ctc_logits": {"dtype": "f32", "shape": ["batch", "time", "classes"]}
            },
            infer=lambda bundle: get_ocr().recognize(bundle),
        )
    )
    formula_loader: FormulaForward | None = None

    def formula_infer(bundle: TensorBundle) -> TensorBundle:
        nonlocal formula_loader
        with lock:
            if formula_loader is None:
                formula_loader = FormulaForward(source_root, config_path, device)
        return formula_loader(bundle)

    registry.register(
        BareModel(
            name="pp_formulanet_plus_m",
            revision="mineru-3.4.5",
            input_schema={"pixel_values": {"dtype": "f32", "shape": ["batch", 1, 384, 384]}},
            output_schema={"token_ids": {"dtype": "i64", "shape": ["batch", "sequence"]}},
            infer=formula_infer,
        )
    )
    table_models: dict[str, OnnxForward] = {}

    def table_infer(
        bundle: TensorBundle,
        name: str,
        model_path: str,
        input_name: str,
        outputs: list[str],
    ) -> TensorBundle:
        with lock:
            model = table_models.get(name)
            if model is None:
                from mineru.utils.models_download_utils import (
                    auto_download_and_get_model_root_path,
                )

                path = Path(auto_download_and_get_model_root_path(model_path)) / model_path
                model = OnnxForward(path, input_name, outputs)
                table_models[name] = model
        result = model(bundle)
        return TensorBundle(
            tensors=result.tensors,
            metadata={"model": name, "revision": "mineru-3.4.5"},
        )

    table_specs = [
        (
            "paddle_table_cls",
            "models/TabCls/paddle_table_cls/PP-LCNet_x1_0_table_cls.onnx",
            "x",
            ["logits"],
            {"x": {"dtype": "f32", "shape": ["batch", 3, 224, 224]}},
            {"logits": {"dtype": "f32", "shape": ["batch", 2]}},
        ),
        (
            "slanet_plus",
            "models/TabRec/SlanetPlus/slanet-plus.onnx",
            "x",
            ["loc_preds", "structure_probs"],
            {"x": {"dtype": "f32", "shape": ["batch", 3, "height", "width"]}},
            {
                "loc_preds": {"dtype": "f32", "shape": ["batch", "steps", 8]},
                "structure_probs": {
                    "dtype": "f32",
                    "shape": ["batch", "steps", "classes"],
                },
            },
        ),
        (
            "unet_table_structure",
            "models/TabRec/UnetStructure/unet.onnx",
            "input",
            ["segmentation"],
            {"input": {"dtype": "f32", "shape": ["batch", 3, "height", "width"]}},
            {
                "segmentation": {
                    "dtype": "i64",
                    "shape": [1, "batch", "height", "width"],
                }
            },
        ),
    ]
    for name, path, input_name, outputs, input_schema, output_schema in table_specs:
        registry.register(
            BareModel(
                name=name,
                revision="mineru-3.4.5",
                input_schema=input_schema,
                output_schema=output_schema,
                infer=lambda bundle, name=name, path=path, input_name=input_name, outputs=outputs: table_infer(
                    bundle, name, path, input_name, outputs
                ),
            )
        )
    return registry
