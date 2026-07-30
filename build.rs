use std::{error::Error, fs, path::Path};

use sha2::{Digest as _, Sha256};

const VAD_MODEL_PATH: &str = "assets/vad/ggml-silero-v6.2.0.bin";
const VAD_MODEL_SIZE: usize = 885_098;
const VAD_MODEL_SHA256: &str = "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987";

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed={VAD_MODEL_PATH}");

    let model = fs::read(Path::new(VAD_MODEL_PATH))?;
    if model.len() != VAD_MODEL_SIZE {
        return Err(format!(
            "内嵌 Silero VAD 模型大小无效：expected={VAD_MODEL_SIZE}, actual={}",
            model.len()
        )
        .into());
    }
    let actual_sha256 = format!("{:x}", Sha256::digest(&model));
    if actual_sha256 != VAD_MODEL_SHA256 {
        return Err(format!(
            "内嵌 Silero VAD 模型 SHA-256 无效：expected={VAD_MODEL_SHA256}, actual={actual_sha256}"
        )
        .into());
    }

    Ok(())
}
