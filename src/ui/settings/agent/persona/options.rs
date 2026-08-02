//! 维护人格表单中模型候选项与稳定 ID 的映射。

use std::{collections::HashSet, path::Path};

use gpui::SharedString;
use lunamate_agent::config::{ModelKind, PersonaSettings, SharedLlmSettings};
use rust_i18n::t;

use super::{super::provider_display_name, Live2dModelOption};

/// 人格绑定供应商的第一项固定表示"跟随全局默认供应商"。
const BOUND_PROVIDER_INHERIT: &str = "\u{2014}";
/// Live2D 绑定的第一项固定表示跟随全局模型设置。
const BOUND_LIVE2D_INHERIT: &str = "\u{2014}";

pub(super) fn model_option_names(
    providers: &SharedLlmSettings,
    kind: ModelKind,
) -> Vec<SharedString> {
    let mut names = Vec::with_capacity(providers.models.len() + 1);
    names.push(SharedString::from(format!(
        "{BOUND_PROVIDER_INHERIT} {}",
        if kind == ModelKind::ChatCompletions {
            t!("persona.provider_inherit")
        } else {
            t!("persona.tts_disabled")
        }
    )));
    for model in providers.models.iter().filter(|model| model.kind == kind) {
        names.push(SharedString::from(format!(
            "{} · {}",
            model.label,
            provider_display_name(model.provider)
        )));
    }
    names
}

pub(super) fn model_option_index(
    providers: &SharedLlmSettings,
    kind: ModelKind,
    bound: Option<&str>,
) -> usize {
    bound
        .and_then(|id| {
            providers
                .models
                .iter()
                .filter(|model| model.kind == kind)
                .position(|model| model.id == id)
        })
        .map_or(0, |index| index + 1)
}

pub(super) fn model_option_id(
    providers: &SharedLlmSettings,
    kind: ModelKind,
    row: usize,
) -> Option<String> {
    row.checked_sub(1)
        .and_then(|index| {
            providers
                .models
                .iter()
                .filter(|model| model.kind == kind)
                .nth(index)
        })
        .map(|model| model.id.clone())
}

pub(super) fn live2d_option_state(
    models: &[Live2dModelOption],
    bound: Option<&Path>,
) -> (Vec<SharedString>, usize, Option<std::path::PathBuf>) {
    let mut names = Vec::with_capacity(models.len() + 2);
    names.push(SharedString::from(format!(
        "{BOUND_LIVE2D_INHERIT} {}",
        t!("persona.live2d_inherit")
    )));
    names.extend(
        models
            .iter()
            .map(|model| SharedString::from(model.label.clone())),
    );
    let Some(bound) = bound else {
        return (names, 0, None);
    };
    if let Some(index) = models.iter().position(|model| model.path == bound) {
        return (names, index + 1, None);
    }
    names.push(SharedString::from(
        t!(
            "persona.live2d_missing_option",
            path = bound.to_string_lossy().into_owned()
        )
        .to_string(),
    ));
    (names, models.len() + 1, Some(bound.to_path_buf()))
}

pub(super) fn next_persona_id(settings: &PersonaSettings, reserved: &HashSet<String>) -> String {
    for index in 1_u64.. {
        let id = format!("persona-{index}");
        if !reserved.contains(&id)
            && !settings.pending_deletions.contains(&id)
            && !settings.personas.iter().any(|persona| persona.id == id)
        {
            return id;
        }
    }
    unreachable!("u64 人格 ID 空间不可能被现有配置耗尽")
}

/// 暴露新人格 ID 分配规则，供测试断言不会与既有条目冲突。
#[cfg(test)]
pub(crate) fn next_persona_id_for_test(settings: &PersonaSettings) -> String {
    next_persona_id(settings, &HashSet::new())
}

/// 暴露绑定供应商选择项与配置 ID 的双向映射，供测试断言往返一致。
#[cfg(test)]
pub(crate) fn provider_option_index_for_test(
    providers: &SharedLlmSettings,
    bound: Option<&str>,
) -> usize {
    model_option_index(providers, ModelKind::ChatCompletions, bound)
}

#[cfg(test)]
pub(crate) fn tts_model_option_index_for_test(
    providers: &SharedLlmSettings,
    bound: Option<&str>,
) -> usize {
    model_option_index(providers, ModelKind::SpeechSynthesis, bound)
}
