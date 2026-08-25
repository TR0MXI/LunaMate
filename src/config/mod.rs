//! 通过根应用私有入口访问 `lunamate-config` 的配置 Store。
//!
//! 配置解析、持久化、revision 和不可变发布状态由独立 crate 拥有；根包只保留
//! 应用级 LazyLock，避免各运行时模块自行创建或发现配置实例。

use std::sync::LazyLock;

pub(crate) use lunamate_config::config::*;

/// 根应用唯一的配置访问入口；测试和可复用代码应直接构造 `LunaConfig`。
pub(crate) static CONFIG: LazyLock<LunaConfig> = LazyLock::new(LunaConfig::load);
