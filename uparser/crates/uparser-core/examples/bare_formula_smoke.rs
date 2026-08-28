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

    let inputs = pipeline_formula::preprocess(&std::fs::read(image_path)?)?;
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
