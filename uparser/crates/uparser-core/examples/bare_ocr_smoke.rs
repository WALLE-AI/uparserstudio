use std::time::Duration;
use uparser_core::pipeline_ocr::{self, CtcDictionary};
use uparser_core::tensor_wire;
use uparser_core::transport::{BinaryRequest, Transport};

async fn infer(
    endpoint: String,
    inputs: &tensor_wire::TensorBundle,
) -> anyhow::Result<tensor_wire::TensorBundle> {
    let response = Transport::new()
        .dispatch_binary(BinaryRequest {
            endpoint,
            body: tensor_wire::encode(inputs)?,
            timeout: Duration::from_secs(180),
            max_retries: 0,
        })
        .await?;
    Ok(tensor_wire::decode(&response)?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let image_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: bare_ocr_smoke IMAGE DICTIONARY [ENDPOINT_BASE]"))?;
    let dictionary = CtcDictionary::from_path(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("missing DICTIONARY"))?,
    )?;
    let base = args
        .next()
        .unwrap_or_else(|| "http://127.0.0.1:19102".to_owned());
    let image = image::load_from_memory(&std::fs::read(image_path)?)?.to_rgb8();
    let detection_input = pipeline_ocr::preprocess_detection(&image)?;
    let detection_output = infer(
        format!("{base}/v1/models/pp_ocrv6_det:infer"),
        &detection_input.tensors,
    )
    .await?;
    let detections =
        pipeline_ocr::decode_detection(&detection_output, image.dimensions(), 0.3, 0.6, 1.5)?;
    let crops = detections
        .iter()
        .map(|detection| pipeline_ocr::crop_detection(&image, detection))
        .collect::<Result<Vec<_>, _>>()?;
    let recognition_input = pipeline_ocr::preprocess_recognition(&crops)?;
    let recognition_output = infer(
        format!("{base}/v1/models/pp_ocrv6_rec:infer"),
        &recognition_input,
    )
    .await?;
    let recognized = pipeline_ocr::decode_ctc(&recognition_output, &dictionary)?;
    for (detection, recognition) in detections.iter().zip(recognized) {
        println!(
            "{:.4}\t{}\t{:?}",
            recognition.confidence, recognition.text, detection.points
        );
    }
    Ok(())
}
