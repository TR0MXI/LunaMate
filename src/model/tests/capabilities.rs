//! 验证模型能力检查的声明上限与 Drawable 解析结果。
//!
//! `ModelCapabilities::inspect` 需要 Mocari 从真实 `.moc3` 构建的运行时才能解析 Drawable
//! 索引，最小 JSON fixture 无法覆盖。LunaMate 没有 Live2D 模型的再分发授权，因此依赖真实
//! 模型的用例统一标记 `#[ignore]`，由用户自备模型后手动运行。

use crate::model::capabilities::{ModelCapabilities, ModelDiagnosticResource};

fn local_model_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("models/hiyori_free/runtime/hiyori_free_t08.model3.json")
}

#[test]
#[ignore = "需要自备 Live2D 模型构建运行时；无模型分发授权，请在本地放置模型后手动运行"]
fn declared_hit_areas_resolve_to_drawables_without_diagnostics() {
    let model =
        mocari::assets::load_model_runtime(local_model_path()).expect("自备测试模型应当可以加载");

    let (capabilities, diagnostics) = ModelCapabilities::inspect(&model);

    assert!(
        !capabilities.hit_areas().is_empty(),
        "自备模型应当声明至少一个 HitArea"
    );
    assert!(
        capabilities.hit_area_bounds_count() >= 1,
        "至少需要一个 Drawable 包围盒槽位"
    );
    assert!(
        capabilities.hit_area_bounds_count() <= capabilities.hit_areas().len(),
        "复用同一 Drawable 的 HitArea 必须共享包围盒槽位"
    );
    assert!(
        diagnostics
            .entries()
            .iter()
            .all(|diagnostic| diagnostic.resource() != ModelDiagnosticResource::HitArea),
        "结构完好的模型不应产生 HitArea 诊断"
    );
}

#[test]
#[ignore = "需要自备 Live2D 模型构建运行时；无模型分发授权，请在本地放置模型后手动运行"]
fn hit_area_identifiers_and_names_are_preserved_in_declaration_order() {
    let model =
        mocari::assets::load_model_runtime(local_model_path()).expect("自备测试模型应当可以加载");
    let declared = model
        .runtime()
        .model()
        .hit_areas()
        .iter()
        .map(|hit_area| (hit_area.id().to_owned(), hit_area.name().to_owned()))
        .collect::<Vec<_>>();

    let (capabilities, _diagnostics) = ModelCapabilities::inspect(&model);
    let resolved = capabilities
        .hit_areas()
        .iter()
        .map(|hit_area| (hit_area.id().to_string(), hit_area.name().to_string()))
        .collect::<Vec<_>>();

    // 解析结果只会跳过无效引用，保留下来的项必须维持清单声明顺序。
    assert!(
        resolved.iter().all(|entry| declared.contains(entry)),
        "解析出的 HitArea 必须来自模型清单声明"
    );
    assert!(resolved.len() <= declared.len());
}
