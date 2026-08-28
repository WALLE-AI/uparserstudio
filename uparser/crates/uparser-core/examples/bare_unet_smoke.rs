use std::time::Duration;
use uparser_core::pipeline_table;
use uparser_core::tensor_wire;
use uparser_core::transport::{BinaryRequest, Transport};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let image_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: bare_unet_smoke IMAGE [ENDPOINT]"))?;
    let endpoint = args.next().unwrap_or_else(|| {
        "http://127.0.0.1:19102/v1/models/unet_table_structure:infer".to_owned()
    });
    let image = image::load_from_memory(&std::fs::read(image_path)?)?.to_rgb8();
    let inputs = pipeline_table::preprocess_unet(&image)?;
    let response = Transport::new()
        .dispatch_binary(BinaryRequest {
            endpoint,
            body: tensor_wire::encode(&inputs)?,
            timeout: Duration::from_secs(180),
            max_retries: 0,
        })
        .await?;
    let outputs = tensor_wire::decode(&response)?;
    let decoded = pipeline_table::decode_unet(&outputs, image.dimensions())?;
    println!("cells={}", decoded.cells.len());
    println!("{}", pipeline_table::render_wired_html(&decoded.cells));
    Ok(())
}
