use std::time::Duration;
use uparser_core::pipeline_layout;
use uparser_core::tensor_wire;
use uparser_core::transport::{BinaryRequest, Transport};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let endpoint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:19002/v1/models/pp_doclayout_v2:infer".into());
    let image_path = std::env::args().nth(2);
    let (inputs, original) = if let Some(path) = image_path {
        pipeline_layout::preprocess(&std::fs::read(path)?, 800, 800)?
    } else {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::new(800, 800));
        let mut encoded = Vec::new();
        image.write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )?;
        pipeline_layout::preprocess(&encoded, 800, 800)?
    };
    let request = tensor_wire::encode(&inputs)?;
    let response = Transport::new()
        .dispatch_binary(BinaryRequest {
            endpoint,
            body: request,
            timeout: Duration::from_secs(180),
            max_retries: 0,
        })
        .await?;
    let bundle = tensor_wire::decode(&response)?;
    for tensor in &bundle.tensors {
        println!("{} {:?} {:?}", tensor.name, tensor.dtype, tensor.shape);
    }
    let detections = pipeline_layout::decode(&bundle, original.0, original.1, 0.45)?;
    println!("{}", serde_json::to_string_pretty(&detections)?);
    Ok(())
}
