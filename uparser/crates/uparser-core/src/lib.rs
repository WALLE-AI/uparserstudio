//! Module visibility policy (O1.3 — see `ARCHITECTURE_OPTIMIZATION_EXECUTION_PLAN.md`).
//!
//! Everything used to be `pub`, which meant `dead_code` could never fire:
//! a module with zero remaining callers (the Pipeline V1 family,
//! `ingest::ingest_document`, `profiler::profile_l2`) still looked alive to
//! the compiler. Only these stay public now:
//!
//! * `api` — the surface both binding crates call.
//! * `cli` — the `uparser` binary's entry point.
//! * `types` / `frontend` / `runner` / `router` / `protocol_spec` — the IR
//!   and orchestration contract those two surfaces expose in their signatures.
//! * `adapters` / `ingest` / `imaging` / `render` / `testing` — reached by the
//!   integration tests in `tests/`, which are separate crates and therefore
//!   can only see `pub` items.
//!
//! `transport`, `tensor_wire` and the four `pipeline_*` decoders are needed
//! by the manual smoke-runner examples only. They are `pub` under the
//! non-default `internals` feature (which `examples/*` declare as required)
//! and `pub(crate)` otherwise, so a normal build still reports them dead if
//! their last in-crate caller disappears.
//!
//! Every other module is `pub(crate)`, so losing its last caller now
//! produces a build warning instead of silently accumulating.

pub mod adapters;
pub(crate) mod agent_config;
pub mod api;
pub(crate) mod ascend;
pub(crate) mod assets;
pub(crate) mod cache;
pub(crate) mod category_map;
pub mod cli;
pub(crate) mod content_normalize;
pub(crate) mod formula_repair;
pub mod frontend;
pub(crate) mod geometry;
pub mod imaging;
pub mod ingest;
pub(crate) mod markdown_ir;
pub(crate) mod otsl;
pub(crate) mod output_parse;
pub(crate) mod page_range;
#[cfg(feature = "internals")]
pub mod pipeline_formula;
#[cfg(not(feature = "internals"))]
pub(crate) mod pipeline_formula;
#[cfg(feature = "internals")]
pub mod pipeline_layout;
#[cfg(not(feature = "internals"))]
pub(crate) mod pipeline_layout;
#[cfg(feature = "internals")]
pub mod pipeline_ocr;
#[cfg(not(feature = "internals"))]
pub(crate) mod pipeline_ocr;
#[cfg(feature = "internals")]
pub mod pipeline_table;
#[cfg(not(feature = "internals"))]
pub(crate) mod pipeline_table;
pub(crate) mod postprocess;
pub(crate) mod profiler;
pub mod protocol_spec;
pub(crate) mod reading_order;
pub mod render;
pub(crate) mod robustness;
pub mod router;
pub mod runner;
pub(crate) mod scheduler;
pub(crate) mod semantic;
pub(crate) mod shape_executor;
pub(crate) mod stage_graph;
pub(crate) mod structured;
#[cfg(feature = "internals")]
pub mod tensor_wire;
#[cfg(not(feature = "internals"))]
pub(crate) mod tensor_wire;
pub mod testing;
#[cfg(feature = "internals")]
pub mod transport;
#[cfg(not(feature = "internals"))]
pub(crate) mod transport;
pub mod types;
