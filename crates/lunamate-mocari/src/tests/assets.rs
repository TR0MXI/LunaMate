use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    RuntimeModelAssets,
    assets::{DecodedTexture, load_model_runtime_from_assets},
    json::Model3,
};

#[test]
fn owned_assets_build_a_runtime_without_reading_manifest_paths() {
    let model = Model3::from_json_str(
        r#"{"Version":3,"FileReferences":{"Moc":"missing.moc3","Textures":["missing.png"]}}"#,
    )
    .expect("固定模型清单应当可以解析");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间必须晚于 Unix 纪元")
        .as_nanos();
    let missing_model_dir = std::env::temp_dir().join(format!(
        "mocari-owned-assets-missing-{}-{nonce}",
        std::process::id()
    ));
    let assets = RuntimeModelAssets::new(
        model,
        minimal_empty_moc(),
        None,
        None,
        vec![DecodedTexture::new(1, 1, vec![1, 2, 3, 4])],
        &missing_model_dir,
    );

    let runtime =
        load_model_runtime_from_assets(assets).expect("owned loader 不应读取清单中的缺失路径");

    assert!(runtime.runtime().meshes().is_empty());
    assert_eq!(runtime.runtime().model().moc(), "missing.moc3");
    assert_eq!(runtime.textures()[0].rgba(), [1, 2, 3, 4]);
    assert_eq!(runtime.model_dir(), Some(missing_model_dir.as_path()));
}

fn minimal_empty_moc() -> Vec<u8> {
    const OFFSET_TABLE_START: usize = 0x40;
    const OFFSET_COUNT: usize = 160;
    const COUNT_INFO_WORDS: usize = 35;
    const U32_SIZE: usize = 4;
    const COUNT_INFO_OFFSET: usize = OFFSET_TABLE_START + OFFSET_COUNT * U32_SIZE;
    const CANVAS_INFO_OFFSET: usize = COUNT_INFO_OFFSET + COUNT_INFO_WORDS * U32_SIZE;
    const CANVAS_INFO_SIZE: usize = 64;

    let mut bytes = vec![0_u8; CANVAS_INFO_OFFSET + CANVAS_INFO_SIZE];
    bytes[0..4].copy_from_slice(b"MOC3");
    bytes[4] = 6;
    bytes[5] = 0;
    bytes[OFFSET_TABLE_START..OFFSET_TABLE_START + U32_SIZE]
        .copy_from_slice(&(COUNT_INFO_OFFSET as u32).to_le_bytes());
    bytes[OFFSET_TABLE_START + U32_SIZE..OFFSET_TABLE_START + U32_SIZE * 2]
        .copy_from_slice(&(CANVAS_INFO_OFFSET as u32).to_le_bytes());
    bytes[CANVAS_INFO_OFFSET..CANVAS_INFO_OFFSET + U32_SIZE]
        .copy_from_slice(&1.0_f32.to_le_bytes());
    bytes[CANVAS_INFO_OFFSET + U32_SIZE * 3..CANVAS_INFO_OFFSET + U32_SIZE * 4]
        .copy_from_slice(&1.0_f32.to_le_bytes());
    bytes[CANVAS_INFO_OFFSET + U32_SIZE * 4..CANVAS_INFO_OFFSET + U32_SIZE * 5]
        .copy_from_slice(&1.0_f32.to_le_bytes());
    bytes
}
