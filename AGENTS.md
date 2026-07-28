# LunaMate 开发指南

本文只记录长期有效的项目约束，避免复制易过期的实现快照：

- 依赖、feature、lint 和构建配置以 `Cargo.toml` 为准，精确版本与 Git commit 以
  `Cargo.lock` 为准。
- 已实现能力以源码为准，计划与验收项以 `TODO.md` 为准；Mocari 来源和本地差异以
  `vendor/mocari/LUNAMATE.md` 为准。

## 项目边界

LunaMate 是基于 Rust、GPUI、Mocari 和 genai 的 Live2D 桌面宠物，仅提供二进制应用；
`src/main.rs` 是唯一 crate 入口，不要新增 `src/lib.rs` 或 `[lib]` target。

- UI/窗口、Live2D 运行时与渲染、互动状态、LLM、语音、配置和持久化保持职责边界。Provider
  细节不得进入 UI，Live2D 层不得发起网络请求，音频层不得访问 Agent 内部会话或 Provider。
- 通用 GPUI 视图、主题和窗口布局通过 `src/ui` façade 暴露，Agent 对话、供应商与人格
  设置视图通过 `src/agent` façade 暴露；原生窗口适配、underlay surface attachment
  与对应 `unsafe` 收口在 `src/platform`。可持久化外观类型属于配置域，不得依赖 UI。
- Live2D 运行时、模型目录、能力、交互命令、帧调度与 CPU/GPU 渲染统一通过
  `src/model` façade 暴露；外部模块不得依赖其动画、表情、资源解析、worker 或 renderer
  子模块。
- 动作、表情与服装的运行时 ID 和原始名称属于模型域，用户显示名与根目录外部表情的
  表情/服装分类属于配置域；设置和 Agent 必须用显示名呈现、用稳定 ID 下发命令，不得通过
  重命名修改第三方模型文件。
- 全局快捷键的系统注册、事件路由与 GPUI 按键转换统一收口在 `src/shortcut.rs`；可持久化的
  动作和按键组合属于配置域。设置录入期间必须释放已有注册，语音按住事件必须有独立于音频
  推进的超时释放兜底。
- 对话能力、供应商与人格设置编辑器和人格记忆收口在 `src/agent`；应用与其他 UI 模块只
  通过 `Agent`、`AgentView`、`AgentShutdown`、`AgentMemoryAccess`、`PersonaMemory` 与两个
  设置编辑器的 `*View`、`*Draft`、`*Event` 交互，不得构造或依赖内部会话、存储、快照、
  记忆记录或 Provider 配置类型。
- 供应商与人格是两个独立配置域：供应商只描述连接与模型调用参数，人格描述身份、系统
  提示词、可选的供应商绑定与自己的记忆。人格 ID 同时是会话文档键与 `agent_memory.agent_id`，
  必须限制为安全字符集，人格列表始终至少保留一条。
- `src/database` 统一提供嵌入式数据库 façade 和配置文件原子替换；外部模块不得依赖
  SurrealDB 类型，调用方负责转换各自的领域错误。
- 模块按稳定职责拆分，跨模块使用明确的状态、事件或接口；避免循环依赖、隐藏全局状态
  和双向调用。公共接口默认最小可见性，不为规划中的能力预建抽象。

## 依赖与第三方代码

- 维护 `Cargo.lock`；新增依赖使用 `cargo add`，不要无目的执行 `cargo update`。引入
  crate 前评估现有方案、维护状态、安全、许可证、测试、体积、线程和系统依赖。
- GPUI 相关 Git 依赖必须兼容并解析到同一 Zed commit，依赖图中只保留一套 GPUI 类型。
  Live2D 代码只使用 `gpui_wgpu::wgpu`，underlay GPU 资源不得传入 GPUI renderer；
  不要用个人 fork 或另一版 GPUI 绕过类型冲突。
- GPUI 的 `test-support` feature 只作为 dev-dependency 启用，用于无头 `TestAppContext`；
  它必须与生产 `gpui` 解析到同一 commit，且不得被生产代码路径依赖。
- `global-hotkey` 只用于 Windows、macOS 与 X11 的系统级快捷键；原生 Wayland 使用 XDG
  GlobalShortcuts portal、稳定应用/动作 ID 和合成器返回的绑定子集，不得把 preferred trigger
  或本地配置伪装为注册成功。发布包必须安装与应用 ID 匹配的 desktop entry。
- `vendor/mocari` 是第三方源码边界；保留 MIT `LICENSE`、原始 manifest 和
  `LUNAMATE.md`，本地修改限于必要的兼容或安全修复，并同步记录差异。升级相关依赖时
  检查 `[patch.crates-io]` 是否可以移除。
- Silero VAD 只使用 whisper-rs 公开安全接口和有界滚动上下文，不 vendor 或 patch
  `whisper-rs-sys`。`raw-api` 只用于转写线程的同步 abort callback；whisper-rs 修复 safe
  callback 的 trampoline 类型与所有权后，必须删除该 feature 和本地 abort FFI。
- Whisper 通用构建不暴露 LunaMate GPU feature：macOS 固定包含 Metal，Windows 和 Linux
  固定包含 Vulkan。CUDA、ROCm 与 Intel SYCL 会直接链接供应商运行库，不得加入通用二进制；
  Windows 和 Linux 运行时由系统或 GPU 驱动提供 Vulkan loader。如需支持供应商后端，必须先
  设计可选后端动态加载与独立产物策略。
- SurrealDB 生产依赖只启用 SurrealKV 与 Rustls，测试才启用 Mem；不要启用远程协议、
  原生 TLS、HTTP、脚本、ML 或其 allocator，除非对应能力已有明确需求和验收。

## 工作区与 Git

- 开始编辑和提交前检查 `git status` 与相关 diff，只修改任务所需文件，保护已有未提交改动。
- 未经明确要求，不执行暂存、提交、推送、分支、标签或 PR；禁止破坏性 Git 操作和绕过仓库策略。
- 提交时只暂存本任务文件，并确认不含密钥、令牌、本地模型、配置或构建产物。

## GPUI 与并发

- 按 GPUI 所有权模型访问 `Entity`、`Window`、`App` 和上下文，不把 UI 引用移入工作线程。
- 渲染、输入回调和 UI 线程不得执行网络、同步文件 I/O、模型解析或其他耗时工作；后台
  结果通过 GPUI 上下文回写，LLM 等 Tokio future 运行于 `gpui_tokio` runtime。
- 后台任务必须可取消或识别 generation/revision。窗口关闭、模型或会话切换、请求替换后
  丢弃迟到结果，不得更新失效实体或资源。
- 高频数据使用有界通道、latest-value 状态、合并通知或限流；不得按 token、鼠标采样或
  动画采样无条件重绘，也不得让生产者无限积压。使用 GPUI Component 前完成初始化并按
  约定用 `Root` 包装窗口根视图。

## Live2D 与平台渲染

- 平台窗口、输入适配与 underlay surface attachment 集中隔离。Windows、macOS 和原生
  Wayland 使用独立 WGPU underlay；X11、不支持的平台，以及 attach、adapter、透明
  Alpha、surface 或 device 失败时永久回退 CPU renderer。
- Wayland underlay 复用 GPUI 的 `wl_display` guest connection 创建 child surface，
  不得销毁或主动 commit parent；缺少协议或 fractional scale 支持时回退 CPU，也不得
  假定客户端可以指定顶层窗口坐标。
- GPU worker 独占 surface、device、queue、Mocari GPU 资源和模型可变状态。UI 只发送有界
  语义命令和合并唤醒，不直接锁模型或等待光栅化；成功 present 后才能发布对应 HitArea。
- 关闭窗口时先取消模型并停止 worker，再释放 surface 和原生 attachment。
- Agent 截屏是默认关闭的隐私权限：Windows、macOS 使用 `xcap`，Linux 使用 XDG
  Screenshot portal。只有已持久化且 revision 仍有效时才能注册和执行工具；捕获后、上传前
  及重试前都要复核授权。截图和用户图片必须有文件、像素与编码上限，不得写入会话数据库、
  配置或日志，portal 临时文件在读取后立即清理。
- 纹理、索引和 UV 在 generation 加载时上传；每帧只更新动态顶点、透明度、颜色和绘制
  状态。GPU 常规路径不得整帧 readback、`RenderImage` 或 GPUI atlas 上传。
- 正确处理 drawable 顺序、clipping mask、反向蒙版、blend mode、采样和预乘 Alpha；
  测试不能只覆盖无蒙版单模型。
- 动画使用单调时钟并限制异常 frame delta；空闲时不得 busy loop，热点路径避免重复解析、
  加载、大块分配和高争用锁。逻辑坐标、物理像素、缩放、光栅和模型坐标必须保持一致。
- 模型、纹理和动作是不可信输入。校验路径边界、文件和纹理大小、解析复杂度及数值，对
  缺失或损坏资源返回可诊断错误，不得 panic。
- 未写入清单的动作和表情只自动发现清单目录根层及直属 `motions/`、`expressions/`，不得
  递归扩大扫描边界；`expressions/` 内表情固定为表情，只有根层外部表情可由用户分类为服装。

## 配置与 LLM

- 默认在用户配置目录保存 `config.toml`，兼容工作目录中的旧文件；每个人格的有界会话仅
  保存到工作目录下固定的 `./data/lunamate.db` 嵌入式数据库。配置写入必须原子化，数据库
  目录和配置文件必须限制本地访问权限。
- Provider、endpoint、模型、模型调用参数和系统提示词来自配置；持久化 LunaMate 稳定
  Provider ID 与思考强度标识，不直接序列化 `AdapterKind` 或 genai 选项类型，也不把用户
  输入当作环境变量名。未显式启用的可选请求参数不得发送，必须沿用 Provider 默认值。
- 记忆按人格隔离为短期上下文、中期条目与长期检索三层；删除人格必须同步删除其全部记忆，
  清除记忆等不可逆操作必须先经用户二次确认。记忆读写只经 `database` façade。
- API key、access token 和 endpoint 凭据只从本地配置读取，并在 UI 中默认遮蔽；不得写入
  源码、Git、日志或错误信息。
- 网络调用必须异步，支持超时、取消和有限退避重试。优先流式响应，按批次刷新 UI；会话
  历史限制消息数和字节或 token 数。
- 日志默认不记录完整对话、系统提示词或隐私数据，诊断信息必须脱敏。LLM 输出是不可信
  输入，不得直接获得 shell、任意文件或系统控制权限；自动化测试使用 fake backend。
- 应用日志统一使用 `log` 门面和 `flexi_logger` 异步文件 writer，固定写入工作目录下的
  `logs/`；轮转、压缩、保留策略和等级由日志配置域管理，Release 构建不做编译期等级裁剪。

## 代码、安全与资源

- 项目维护的 Rust 源码和手写配置注释使用简体中文；标识符、API 名和 `SAFETY` 等固定
  标签可保留原文，生成文件与第三方代码除外。
- 模块使用 `//!` 说明职责，跨模块稳定接口和复杂契约使用 `///`。状态转换、取消与过期
  处理、同步、资源生命周期、坐标变换和渲染不变量应说明原因，不复述代码。
- 仅含单个源文件的模块使用同级 `name.rs`；只有模块需要拆分子模块时才使用
  `name/mod.rs` 目录结构，集中测试目录除外。
- 单元测试集中在所属顶级模块的 `tests/mod.rs`，并通过 `tests/xxx.rs` 对应被测子模块；
  生产文件不保留内联 `mod tests`，也不为测试把内部实现扩大到顶级 façade 之外。
- 为测试新增的构造器和访问器使用 `#[cfg(test)]` 并保持模块内可见性，命名以 `_for_test`
  结尾；不得因此放宽生产可见性或改变生产行为。
- `unsafe` 仅用于必要的平台、GPU 互操作，以及 `src/voice/transcribe.rs` 的同步推理 abort
  callback，范围保持最小；每个块紧邻 `// SAFETY:`，用中文说明可验证的安全前置条件。清理
  失效注释、注释掉的代码和无跟踪项的模糊 `TODO`。
- 遵守 `clippy::unwrap_used = "deny"`。使用 `?` 或显式分支处理失败；`expect()` 仅用于
  已证明的不变量或测试前置条件，并写明条件。网络、配置、模型和平台错误应带上下文且
  可恢复，单个子系统失败不得拖垮应用。
- `models/` 仅用于本地测试，不纳入版本库。第三方代码、模型、纹理、图标、音频和字体
  必须记录来源与许可证；再分发权不明确的资源不得进入仓库或发布包。

## 开发与验证

完成 Rust 代码变更后，依次执行并修复所有问题：

```bash
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all
cargo test --locked
```

- 目标平台系统依赖导致命令失败时，记录准确原因，不得删除 feature、测试或错误处理来掩盖问题。
- 涉及窗口、透明度、缩放、鼠标命中或 GPU 渲染的变更，必须在真实目标桌面环境验证。
- 实时状态测试覆盖取消、超时、迟到或乱序结果、大 frame delta、模型缺失和窗口关闭。
- GPUI 视图测试使用无头 `TestAppContext`，且必须保持确定性：不得依赖 `gpui_tokio` 等
  测试线程之外的唤醒，否则测试调度器会判定为非确定性。跨 runtime 的流式行为改在
  Provider 服务层用 fake backend 覆盖。
- 依赖真实 Live2D 模型或桌面截屏授权的用例标注 `#[ignore]` 并写明原因；仓库不分发模型，
  由使用者自备模型后手动运行。
- 依赖真实麦克风、Whisper 或 Silero 模型、GPU 后端的用例同样标注 `#[ignore]`；常规测试用
  fake VAD 和纯状态机覆盖端点、取消与迟到结果，仓库不分发语音模型。
- 修改依赖策略、平台支持、目录约定或关键架构时同步更新本文件；具体计划写入 `TODO.md`。
