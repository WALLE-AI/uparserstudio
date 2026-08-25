"""Pydantic mirror of uparser-core's pipeline V2 wire contract."""

from __future__ import annotations

from typing import Generic, Literal, TypeVar

from pydantic import BaseModel, ConfigDict, Field, model_validator


SCHEMA_VERSION = "uparser.pipeline.v2"
CoordinateSpace = Literal["render_pixels", "page_points"]


class ContractModel(BaseModel):
    model_config = ConfigDict(extra="forbid")


class ImageDimensions(ContractModel):
    width: int = Field(gt=0)
    height: int = Field(gt=0)


class EncodedImage(ContractModel):
    media_type: Literal["image/png", "image/jpeg"]
    base64_data: str = Field(min_length=1)


class PageImage(ContractModel):
    page_id: str = Field(min_length=1)
    image: EncodedImage
    dimensions: ImageDimensions
    rotation_degrees: Literal[0, 90, 180, 270] = 0


class Polygon(ContractModel):
    points: list[tuple[float, float]] = Field(min_length=3)


class Region(ContractModel):
    region_id: str = Field(min_length=1)
    label: str = Field(min_length=1)
    bbox: tuple[float, float, float, float]
    polygon: Polygon | None = None
    confidence: float | None = Field(default=None, ge=0, le=1)
    coordinate_space: CoordinateSpace

    @model_validator(mode="after")
    def ordered_bbox(self):
        x0, y0, x1, y1 = self.bbox
        if x1 <= x0 or y1 <= y0:
            raise ValueError("bbox coordinates must be ordered")
        return self


class ModelMetadata(ContractModel):
    name: str
    revision: str
    weight_sha256: str | None = None
    runtime: str


class StageWarning(ContractModel):
    code: str
    message: str
    region_id: str | None = None


class StageError(ContractModel):
    code: str
    message: str
    retryable: bool
    region_id: str | None = None


ItemT = TypeVar("ItemT")
ResultT = TypeVar("ResultT")


class BatchRequest(ContractModel, Generic[ItemT]):
    schema_version: Literal[SCHEMA_VERSION]
    request_id: str = Field(min_length=1)
    items: list[ItemT] = Field(min_length=1)


class BatchItemResult(ContractModel, Generic[ResultT]):
    page_id: str
    result: ResultT | None = None
    error: StageError | None = None
    warnings: list[StageWarning] = Field(default_factory=list)

    @model_validator(mode="after")
    def exactly_one_outcome(self):
        if (self.result is None) == (self.error is None):
            raise ValueError("exactly one of result or error must be set")
        return self


class BatchResponse(ContractModel, Generic[ResultT]):
    schema_version: Literal[SCHEMA_VERSION] = SCHEMA_VERSION
    request_id: str
    model: ModelMetadata
    items: list[BatchItemResult[ResultT]]


class LayoutResult(ContractModel):
    regions: list[Region]


class FormulaDetectionResult(ContractModel):
    regions: list[Region]


class OcrPageInput(ContractModel):
    page: PageImage
    layout_regions: list[Region]
    formula_regions: list[Region] = Field(default_factory=list)
    language: str


class OcrSpan(ContractModel):
    span_id: str
    parent_region_id: str | None = None
    polygon: Polygon
    text: str
    language: str | None = None
    confidence: float | None = Field(default=None, ge=0, le=1)
    coordinate_space: CoordinateSpace


class OcrResult(ContractModel):
    spans: list[OcrSpan]


class FormulaRecognitionInput(ContractModel):
    page: PageImage
    formula_regions: list[Region]


class FormulaSpan(ContractModel):
    region_id: str
    latex: str
    confidence: float | None = Field(default=None, ge=0, le=1)
    bbox: tuple[float, float, float, float] | None = None


class FormulaRecognitionResult(ContractModel):
    spans: list[FormulaSpan]


class TableRecognitionInput(ContractModel):
    page: PageImage
    table_regions: list[Region]
    ocr_spans: list[OcrSpan]
    formula_spans: list[FormulaSpan] = Field(default_factory=list)


class TableCell(ContractModel):
    cell_id: str
    row: int = Field(ge=0)
    column: int = Field(ge=0)
    row_span: int = Field(gt=0)
    column_span: int = Field(gt=0)
    bbox: tuple[float, float, float, float] | None = None
    text: str
    confidence: float | None = Field(default=None, ge=0, le=1)


class RecognizedTable(ContractModel):
    region_id: str
    html: str
    structure_tokens: list[str]
    cells: list[TableCell]
    classifier_label: str | None = None
    rotation_degrees: Literal[0, 90, 180, 270] = 0
    confidence: float | None = Field(default=None, ge=0, le=1)


class TableRecognitionResult(ContractModel):
    tables: list[RecognizedTable]


class PageAnalyzeInput(ContractModel):
    page: PageImage
    language: str
    formula_enabled: bool
    table_enabled: bool


class PageAnalyzeResult(ContractModel):
    regions: list[Region]
    ocr_spans: list[OcrSpan]
    formula_spans: list[FormulaSpan]
    tables: list[RecognizedTable]
    reading_order: list[str]
    # Full-pipeline backends may provide their own finalized Markdown.  Clients
    # should prefer it over reconstructing content from stage-level geometry.
    markdown: str | None = None


LayoutBatchRequest = BatchRequest[PageImage]
LayoutBatchResponse = BatchResponse[LayoutResult]
FormulaDetectionBatchRequest = BatchRequest[PageImage]
FormulaDetectionBatchResponse = BatchResponse[FormulaDetectionResult]
OcrBatchRequest = BatchRequest[OcrPageInput]
OcrBatchResponse = BatchResponse[OcrResult]
FormulaRecognitionBatchRequest = BatchRequest[FormulaRecognitionInput]
FormulaRecognitionBatchResponse = BatchResponse[FormulaRecognitionResult]
TableRecognitionBatchRequest = BatchRequest[TableRecognitionInput]
TableRecognitionBatchResponse = BatchResponse[TableRecognitionResult]
PageAnalyzeBatchRequest = BatchRequest[PageAnalyzeInput]
PageAnalyzeBatchResponse = BatchResponse[PageAnalyzeResult]
