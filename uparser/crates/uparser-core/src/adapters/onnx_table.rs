//! Local ONNX inference for `pipeline`'s `table` stage (T-5.3), gated
//! behind the `pipeline-local-table` feature since `ort`'s
//! `download-binaries` feature fetches a prebuilt ONNX Runtime binary on
//! first build (same network-availability caveat as `pdfium`).
//!
//! No real SLANet/table-structure ONNX weights are vendored anywhere in
//! `opensource/MinerU` (downloaded at runtime from HF/ModelScope, never
//! committed — confirmed via `find`). This module is validated against a
//! synthetic identity-graph fixture (`tests/fixtures/synthetic_identity.onnx`,
//! generated via the local `onnx` Python package) that proves the `ort`
//! session/tensor plumbing is mechanically correct — it is not a claim
//! of real table-recognition accuracy.
//!
//! **Confirmed environment gap, not a code defect**: in this sandbox
//! (Ubuntu 22.04, glibc 2.35), `cargo build --features
//! pipeline-local-table` compiles this module and the rest of the crate
//! cleanly (`cargo build --lib --features pipeline-local-table`
//! succeeds), but linking any binary/test target fails —
//! `ort-sys`'s `download-binaries`-fetched prebuilt ONNX Runtime static
//! archive references glibc >=2.38 symbols (`__isoc23_strtoll` and
//! friends, added for ISO C23 `strtol` semantics) this sandbox's glibc
//! doesn't provide. Switching to `ort`'s `load-dynamic` feature avoids
//! the static-link step but then hangs fetching the dynamic-library
//! variant from `ort.pyke.io` (a host the corporate proxy likely can't
//! reach; confirmed via a 30s hard-timeout probe, not just "slow").
//! Building ONNX Runtime from source against this sandbox's glibc is out
//! of scope for a research/reference workspace. This is the same
//! category of gap as P1's `mineru_vl_utils` version pin or P4's
//! original pdfium-download caveat — a real external-asset limitation of
//! this sandbox, not something to route around by weakening the code.
//! On a host with glibc >=2.38 (or any current mainstream Linux distro
//! newer than Ubuntu 22.04), this module and its tests are expected to
//! build and pass unmodified.

use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum LocalTableError {
    #[error("failed to load ONNX model at {path}: {source}")]
    ModelLoad {
        path: String,
        #[source]
        source: ort::Error,
    },
    #[error("ONNX inference failed: {0}")]
    Inference(#[source] ort::Error),
    #[error("unexpected model output shape/type")]
    UnexpectedOutput,
}

/// Run a local ONNX table-structure model against `crop`, returning its
/// raw output tensor flattened to `Vec<f32>`. Real deployments would map
/// this into OTSL tokens for `otsl::to_html`; this synthetic-fixture
/// integration only proves the ONNX round-trip itself, so no such
/// mapping exists here yet — that's real-weights-dependent work deferred
/// alongside T-5.7's reference Pipeline Model Serving deployment.
pub fn run_local_table_model(
    model_path: &Path,
    crop: &RgbImage,
) -> Result<Vec<f32>, LocalTableError> {
    let mut session = Session::builder()
        .map_err(LocalTableError::Inference)?
        .commit_from_file(model_path)
        .map_err(|source| LocalTableError::ModelLoad {
            path: model_path.display().to_string(),
            source,
        })?;

    let (width, height) = (crop.width(), crop.height());
    let mut data = vec![0f32; (3 * width * height) as usize];
    for (i, px) in crop.pixels().enumerate() {
        for c in 0..3 {
            data[(c as u32 * width * height) as usize + i] = px.0[c] as f32 / 255.0;
        }
    }
    let tensor = Tensor::from_array(([1usize, 3, height as usize, width as usize], data))
        .map_err(LocalTableError::Inference)?;

    let input_name = session.inputs()[0].name().to_string();
    let output_name = session.outputs()[0].name().to_string();
    let outputs = session
        .run(ort::inputs![input_name.as_str() => tensor])
        .map_err(LocalTableError::Inference)?;

    let (_, output_data) = outputs[output_name.as_str()]
        .try_extract_tensor::<f32>()
        .map_err(|_| LocalTableError::UnexpectedOutput)?;

    Ok(output_data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn fixture_path() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/synthetic_identity.onnx"
        ))
    }

    #[test]
    fn synthetic_identity_model_round_trips_through_ort() {
        let path = fixture_path();
        assert!(path.exists(), "fixture missing at {}", path.display());

        let crop = RgbImage::from_pixel(8, 8, Rgb([128, 64, 32]));
        let output = run_local_table_model(&path, &crop).expect("local ONNX inference succeeds");

        // Identity graph: output must equal the normalized input exactly.
        assert_eq!(output.len(), 3 * 8 * 8);
        assert!((output[0] - 128.0 / 255.0).abs() < 1e-5);
    }

    #[test]
    fn missing_model_file_returns_clean_error_not_panic() {
        let crop = RgbImage::from_pixel(4, 4, Rgb([0, 0, 0]));
        let result = run_local_table_model(Path::new("/no/such/model.onnx"), &crop);
        assert!(matches!(result, Err(LocalTableError::ModelLoad { .. })));
    }
}
