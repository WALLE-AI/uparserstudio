use std::time::Duration;
use uparser_core::pipeline_formula::{self, FormulaDecoder};
use uparser_core::tensor_wire;
use uparser_core::transport::{BinaryRequest, Transport};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let image_path = args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: bare_formula_smoke IMAGE INFERENCE_YAML [ENDPOINT]")
    })?;
    let tokenizer_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing INFERENCE_YAML"))?;
    let endpoint = args.next().unwrap_or_else(|| {
        "http://127.0.0.1:19002/v1/models/pp_formulanet_plus_m:infer".to_owned()
    });

    let image_bytes = std::fs::read(image_path)?;
    let image_bytes = if let Ok(raw_bbox) = std::env::var("UPARSER_FORMULA_BBOX") {
        let bbox = raw_bbox
            .split(',')
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()?;
        if bbox.len() != 4 || bbox[2] <= bbox[0] || bbox[3] <= bbox[1] {
            anyhow::bail!("UPARSER_FORMULA_BBOX must be left,top,right,bottom");
        }
        let image = image::load_from_memory(&image_bytes)?.to_rgb8();
        let crop = image::imageops::crop_imm(
            &image,
            bbox[0],
            bbox[1],
            bbox[2] - bbox[0],
            bbox[3] - bbox[1],
        )
        .to_image();
        let mut encoded = Vec::new();
        image::DynamicImage::ImageRgb8(crop).write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )?;
        encoded
    } else {
        image_bytes
    };
    let inputs = pipeline_formula::preprocess(&image_bytes)?;
    let response = Transport::new()
        .dispatch_binary(BinaryRequest {
            endpoint,
            body: tensor_wire::encode(&inputs)?,
            timeout: Duration::from_secs(180),
            max_retries: 0,
        })
        .await?;
    let outputs = tensor_wire::decode(&response)?;
    for tensor in &outputs.tensors {
        eprintln!("{} {:?} {:?}", tensor.name, tensor.dtype, tensor.shape);
    }
    let decoder = FormulaDecoder::from_inference_yaml(tokenizer_path)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&decoder.decode(&outputs)?)?
    );
    Ok(())
}
