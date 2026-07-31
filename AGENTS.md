# LunaMate 开发指南

本文只记录长期有效的项目约束，避免复制易过期的实现快照：

- 依赖、feature 和 lint 以 `Cargo.toml` 为准，精确版本与 Git commit 以 `Cargo.lock`
  为准；第三方原生库的跨机器构建环境以 `.cargo/config.toml` 为准。
- 已实现能力以源码为准，计划与验收项以 `TODO.md` 为准；Mocari 来源和本地差异以
  `crates/lunamate-mocari/LUNAMATE.md` 为准。

## 快速原型阶段

LunaMate 尚未发布任何版本。首个公开版本发布前，配置、数据库、会话快照和内部 API
都不提供向后兼容保证：

- 修改持久化结构或内部接口时直接更新唯一的当前格式，不保留旧字段读取、迁移器、别名、
  旧路径探测、废弃接口包装或只验证旧格式的测试；开发者自行删除并重建本地原型数据。
- 不为尚未发布或没有外部消费者的代码预留兼容层。只有已经发布的格式、明确存在的外部
  消费者，或任务显式要求时，才能引入迁移与兼容逻辑，并必须写明移除条件。
- 当前格式的缺省值、无效输入诊断、损坏数据恢复、安全校验，以及平台或运行时能力回退
  不属于旧版本兼容，不得以清理兼容代码为由删除。
- 为当前依赖缺陷保留的补丁或适配必须确有构建、正确性或安全需要，记录上游状态与移除条件；
  上游条件满足后立即删除，不把临时补丁演变为长期兼容层。

## 项目边界

LunaMate 是基于 Rust、GPUI、Mocari 和 genai 的 Live2D 桌面宠物。根包 `lunamate` 只提供
二进制应用，`src/main.rs` 是其唯一入口，不要为根包新增 `src/lib.rs` 或 `[lib]` target；
可独立嵌入的 Agent 能力由工作空间成员 `crates/lunamate-agent` 提供。

- UI/窗口、Live2D 运行时与渲染、互动状态、LLM、语音、配置和持久化保持职责边界。Provider
  细节不得进入 UI，Live2D 层不得发起网络请求，音频层不得访问 Agent 内部会话或 Provider。
- 通用 GPUI 视图、主题和窗口布局通过 `src/ui` façade 暴露；Agent 对话适配器位于
  `src/ui/agent`，供应商与人格编辑器位于 `src/ui/settings/agent`。原生窗口适配、屏幕捕获、
  用户图片读取、underlay surface attachment 与对应 `unsafe` 收口在 `src/platform`。
  可持久化外观类型属于配置域，不得依赖 UI。
- Live2D 运行时、模型目录、能力、交互命令、帧调度与 CPU/GPU 渲染统一通过
  `src/model` façade 暴露；外部模块不得依赖其动画、表情、资源解析、worker 或 renderer
  子模块。
- 动作、表情与服装的运行时 ID 和原始名称属于模型域，用户显示名与根目录外部表情的
  表情/服装分类属于配置域；设置和 Agent 必须用显示名呈现、用稳定 ID 下发命令，不得通过
  重命名修改第三方模型文件。
- 全局快捷键的系统注册、事件路由与 GPUI 按键转换统一收口在 `src/shortcut.rs`；可持久化的
  动作和按键组合属于配置域。设置录入期间必须释放已有注册，语音按住事件必须有独立于音频
  推进的超时释放兜底。
- `crates/lunamate-agent` 只拥有框架无关的 Agent 配置领域、会话状态机、Provider 服务、图片
  规范化、记忆访问和持久化协议；不得依赖 GPUI、窗口、文件选择器、桌面 portal、`xcap`、
  根包配置或数据库。`Agent` 及其运行时输入、快照和关键消息类型由 crate 根门面提供，配置、
  媒体、记忆、持久化和工具领域保留命名模块；Provider 执行、session 状态机和 store 协调器是
  私有实现，不得向宿主泄漏。
- `genai` 只负责 Chat Completions。云端 Transcription 与 Speech Synthesis 分别通过
  `lunamate-agent` 的 `stt`、`tts` 门面执行，Provider 协议细节保持私有；宿主只传入经过校验的
  模型配置与有界 PCM/文本，并接收文本或统一 24 kHz PCM。本地 Whisper 推理继续属于根语音层，
  TTS 设备播放继续属于根音频层，两者不得把 Provider 请求带入实时采集或播放回调。
- Transcription 运行时模型由模型目录的 STT 选中项唯一决定，语音配置只保存录音模式；本地
  Whisper 的 GPU 偏好与目标语言属于各自模型条目，目标语言缺省时必须向 whisper-rs 传入
  `None` 以启用自动语种识别。
- `Agent` 直接组合可热更新的 `genai::Client`、`ModelIden`、`ChatOptions`、system prompt 与
  `AgentMemory`。Client 并发使用不加锁，运行时替换通过短持有的统一 `RwLock` 发布，请求启动后
  使用自己的不可变 clone，任何同步锁不得跨网络、工具或持久化 `await`。
- 根配置层仍用带单调 generation 的 `AgentConfigSnapshot` 保证自身域一致性，但只在宿主适配层
  将其解析为 Client、模型、options、prompt、memory 等直接组件；`Agent` 公共 API 不依赖该快照
  类型。配置编辑器直接写根配置层，不得重新引入拉取全局状态的配置回调或 service locator。
  截图授权与捕获通过每次请求独立构造的 `ScreenshotCapability` 注入，会话加载、保存、删除和
  记忆统计、清理通过完整的 `AgentPersistenceCallbacks` 注册。会话排序、取消与迟到 revision
  隔离由 Agent 逻辑负责。
- 根 UI 只负责 GPUI 实体、输入、布局、动画和后台任务接驳；不得复制 Provider adapter、请求
  构建、会话裁剪或文档编码逻辑。设置页使用显示名呈现配置，但运行时能力请求必须携带稳定 ID。
- 供应商与人格是两个独立 Agent 配置域，其配置模型由 `lunamate-agent` 单一拥有；Provider
  与思考强度直接复用并经该 crate 重新导出 genai 类型，不得复制同构枚举。根配置层只负责
  TOML 解析、原子写入与快照发布。供应商只描述连接与模型调用参数，人格描述身份、系统
  提示词、可选的供应商绑定与自己的记忆。人格 ID 同时是会话文档键与 `agent_memory.agent_id`，
  必须限制为安全字符集，人格列表始终至少保留一条。
- 人格绑定的 Live2D 模型只持久化 `models/` 下经过校验的相对清单路径；切换人格时通过
  `src/model` façade 解析并切换。绑定缺失时保留原值并回退有效全局模型，不得静默清除绑定
  或把人格选择反写为全局模型选择。
- `src/database` 统一提供嵌入式数据库 façade 和配置文件原子替换；外部模块不得依赖
  SurrealDB 类型。只有根应用组合层把数据库操作注册为 Agent 持久化回调，并负责转换领域错误。
- 模块按稳定职责拆分，跨模块使用明确的状态、事件或接口；避免循环依赖、隐藏全局状态
  和双向调用。公共接口默认最小可见性，不为规划中的能力预建抽象。

## 依赖与第三方代码

- 维护 `Cargo.lock`；新增依赖使用 `cargo add`，不要无目的执行 `cargo update`。引入
  crate 前评估现有方案、维护状态、安全、许可证、测试、体积、线程和系统依赖。
- 项目维护的工作空间成员共同使用的依赖必须在根 `[workspace.dependencies]` 固定来源、版本与
  基础 feature，成员清单通过 `workspace = true` 继承，避免 GPUI、Tokio、serde 等共享类型出现
  版本分叉；保留原始 manifest 的 `crates/lunamate-mocari` 除外。
- GPUI 相关 Git 依赖必须兼容并解析到同一 Zed commit，依赖图中只保留一套 GPUI 类型。
  Live2D 代码只使用 `gpui_wgpu::wgpu`，underlay GPU 资源不得传入 GPUI renderer；
  不要用个人 fork 或另一版 GPUI 绕过类型冲突。
- GPUI 的 `test-support` feature 只作为 dev-dependency 启用，用于无头 `TestAppContext`；
  它必须与生产 `gpui` 解析到同一 commit，且不得被生产代码路径依赖。
- `global-hotkey` 只用于 Windows、macOS 与 X11 的系统级快捷键；原生 Wayland 使用 XDG
  GlobalShortcuts portal、稳定应用/动作 ID 和合成器返回的绑定子集，不得把 preferred trigger
  或本地配置伪装为注册成功。发布包必须安装与应用 ID 匹配的 desktop entry。
- `crates/lunamate-mocari` 是仓库内维护的第三方源码边界；保留 MIT `LICENSE`、原始 manifest 和
  `LUNAMATE.md`，本地修改限于当前集成、安全或性能所必需的修复，并同步记录差异。升级相关依赖时
  检查 `[patch.crates-io]` 是否可以移除。
- `crates/lunamate-mocari` 同时是非默认 workspace 成员，以便不构建 LunaMate 的语音和桌面依赖就能独立
  运行库测试与 Criterion 基准；根目录默认命令覆盖 `lunamate` 与 `lunamate-agent`，不自动运行
  Mocari 基准。`benchmark-support` 只能用于合成数据基准，不得进入生产依赖 feature。
- Silero VAD v6.2.0 模型作为记录来源和 MIT 许可证的固定资源内嵌进最终二进制，自动模式不得
  要求用户下载或配置 VAD 路径；运行时只使用 whisper-rs 公开安全接口和有界滚动上下文，不
  vendor 或 patch `whisper-rs-sys`。同步转写通过 whisper-rs 公开的 unsafe setter 安装有界生命周期的 abort
  callback；whisper-rs 修复 safe callback 的 trampoline 类型与所有权后，必须删除本地 abort FFI。
- Whisper 通用构建不暴露 LunaMate GPU feature：macOS 固定包含 Metal，Windows 和 Linux
  固定包含 Vulkan。CUDA、ROCm 与 Intel SYCL 会直接链接供应商运行库，不得加入通用二进制；
  Windows 和 Linux 运行时由系统或 GPU 驱动提供 Vulkan loader。如需支持供应商后端，必须先
  设计可选后端动态加载与独立产物策略。
- ggml 必须关闭 `GGML_NATIVE`，避免发布产物绑定构建机器的本地 CPU 指令集，也避免共享编译
  缓存把本机优化对象复用于不同型号的 CI runner；若上游改为默认生成可移植的运行时分派代码，
  可复核并移除该环境约束。
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

- 默认在用户配置目录保存 `config.toml`，不探测或迁移工作目录中的旧文件；每个人格的有界会话仅
  保存到工作目录下固定的 `./data/lunamate.db` 嵌入式数据库。配置写入必须原子化，数据库
  目录和配置文件必须限制本地访问权限。
- Provider、endpoint、模型、模型调用参数和系统提示词来自配置；持久化 ID 映射由 LunaMate
  显式维护，不直接使用 `AdapterKind` 或 genai 选项类型的序列化格式，也不把用户
  输入当作环境变量名。未显式启用的可选请求参数不得发送，必须沿用 Provider 默认值。
- Agent 配置校验、会话协议提示、媒体错误和 Provider 终态必须使用配置快照或单次请求携带的
  `AppLanguage`，不得读取进程全局 locale。外观语言只有在配置持久化并发布成功后才能应用和
  通知运行时，单次请求启动后继续使用自己的语言快照。
- 记忆按人格隔离为短期上下文、中期条目与长期检索三层；删除人格必须同步删除其全部记忆，
  并在配置中保留待清理 ID 的 tombstone，清理完成且移除 tombstone 前不得复用该 ID。清除
  记忆等不可逆操作必须先经用户二次确认。记忆读写只经 `database` façade。
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
- 单元测试集中在所属顶级模块或 crate 的 `tests/mod.rs`，并通过 `tests/xxx.rs` 对应被测子模块；
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

Mocari 变形器 scratch 的时间与分配基准分别执行：

```bash
cargo bench --locked -p mocari --bench deformer_composition --features benchmark-support
cargo bench --locked -p mocari --bench deformer_composition_allocations --features benchmark-support
```

- 目标平台系统依赖导致命令失败时，记录准确原因，不得删除 feature、测试或错误处理来掩盖问题。
- 涉及窗口、透明度、缩放、鼠标命中或 GPU 渲染的变更，必须在真实目标桌面环境验证。
- 实时状态测试覆盖取消、超时、迟到或乱序结果、大 frame delta、模型缺失和窗口关闭。
- GPUI 视图测试使用无头 `TestAppContext`，且必须保持确定性：不得依赖 `gpui_tokio` 等
  测试线程之外的唤醒，否则测试调度器会判定为非确定性。跨 runtime 的流式行为改在
  Agent 核心或私有 Provider 执行层用确定性驱动覆盖，不为测试重新公开 backend trait。
- 依赖真实 Live2D 模型或桌面截屏授权的用例标注 `#[ignore]` 并写明原因；仓库不分发模型，
  由使用者自备模型后手动运行。
- 依赖真实麦克风、Whisper 模型或 GPU 后端的用例同样标注 `#[ignore]`；内嵌 Silero VAD 的资源
  完整性和 CPU 初始化可以常规测试，端点、取消与迟到结果继续用 fake VAD 和纯状态机覆盖。
  仓库不分发 Whisper 模型。
- 修改依赖策略、平台支持、目录约定或关键架构时同步更新本文件；具体计划写入 `TODO.md`。
