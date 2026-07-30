# LunaMate

[English](https://github.com/TR0MXI/LunaMate/blob/master/README_EN.md) | 简体中文

LunaMate 是一款使用 Rust 构建的跨平台 Live2D 桌面宠物。项目以 GPUI 提供桌面窗口和
设置界面，以 Mocari 驱动 Cubism 模型，并通过 genai 接入本地或云端语言模型。

> [!WARNING]
> LunaMate 尚处于快速原型阶段，尚未发布任何版本。首个公开版本前，配置格式、数据库、
> 会话快照和内部接口可能直接发生破坏性变更，不提供旧数据迁移或向后兼容保证。更新源码后，
> 可能需要删除本地 `config.toml` 和 `data/lunamate.db` 并重新配置。

项目仍在开发中，目前包含以下能力：

- Live2D 模型渲染、动作、表情、视线跟随，以及按 HitArea 部位触发的 Agent 互动。
- Windows、macOS 和 Wayland 独立 GPU underlay，以及 X11 或 GPU 不可用时的 CPU 回退。
- 可配置 Provider、模型和 endpoint 的流式多模态 LLM 对话、图片输入，以及有界会话持久化。
- 基于 CPAL、Silero VAD 和 whisper.cpp 的本地语音输入、自动端点检测与对话打断。
- 默认关闭且需要用户在 Tool 设置中显式授权的 Agent 屏幕截图工具。
- 模型、外观、窗口、语言、全局快捷键、LLM 和 Tool 设置界面。

## 构建

先从 [Rust 官方安装页面](https://www.rust-lang.org/tools/install) 安装最新 stable Rust。
推荐使用 `rustup`，安装后会同时获得 `rustc` 和 `cargo`。

还需准备对应平台的原生工具链和 CMake：Windows 使用 MSVC Build Tools、Windows SDK 和
Vulkan SDK，macOS 使用 Xcode Command Line Tools，Linux 使用 C/C++ 工具链、`pkg-config`、
ALSA 开发库，以及 Wayland、X11、Vulkan 开发库和 `glslc`。

Linux 上使用截屏工具或原生 Wayland 全局快捷键时，桌面环境还需提供
`xdg-desktop-portal` 及支持对应接口的后端；首次使用可能显示系统授权确认。Linux 发布包需将
`assets/linux/io.github.tr0mxi.lunamate.desktop` 安装到系统或用户的 applications 目录，
使 host portal 能稳定识别应用并恢复已授权的快捷键。

在仓库根目录执行开发构建：

```bash
cargo build --locked
```

构建优化版本或直接运行：

```bash
cargo build --release --locked
cargo run --release --locked
```

优化后的可执行文件位于 `target/release/lunamate`，Windows 下为
`target/release/lunamate.exe`。

仓库不包含 Live2D 模型。请将具有合法使用授权、包含 `.model3.json` 清单的模型放入
`models/`，再从设置界面选择模型。

### 外部动作、表情与参数服装

除 `.model3.json` 已声明的资源外，LunaMate 还会检查清单所在目录的直属文件，以及以下
直属专属目录，不递归扫描更深层级：

- 根目录和 `motions/` 中的 `*.motion3.json` 会作为独立动作加载。VTube Studio 录制导出的
  Cubism 3 动作可以直接放入这些位置；外部动作按一次性预览播放，结束后恢复 `Idle`。
- 根目录和 `expressions/` 中的 `*.exp3.json` 会作为表情加载。`expressions/` 内的文件固定为
  表情；根目录文件默认显示在表情区，可通过拖动手柄移入服装区，作为 VTube Studio 常见的
  参数换装表达式使用，也可拖回表情区。

新增文件后在模型设置页选择“重新扫描”。动作、表情、参数服装和完整 `.model3.json` 服装
变体都可在该页面重命名；名称只保存在 LunaMate 的 `config.toml` 中，不会改动模型文件。
换装名称也会同步到 Agent 的 `change_outfit` 工具。实际播放仍使用稳定的内部资源 ID，因此
重命名不会破坏预览或换装目标。动作和表情依赖当前模型的参数 ID，来自其他模型的文件可能
成功解析但不会产生预期画面。

## 语音输入

语音设置默认关闭。LunaMate 已将 Silero VAD v6.2.0 内嵌进应用，自动模式启用后直接使用，
无需用户下载或选择 VAD 模型。使用语音输入前，在模型设置的 STT 列表添加并选中一个
Transcription 模型；语音设置页只控制录音模式。本地转写模型使用 whisper.cpp GGML 格式。

自动模式持续监听并在检测到一句话后自动转写、提交，同时允许按住“语音输入”全局快捷键
接管当前候选或活动录音，松开并转写后恢复自动监听；按住说话模式只在按住该快捷键时采集。
录音时主窗口底部显示音量波形。若用户在模型流式回复期间开始说话，当前回复会立即停止，
其已生成部分会带“被用户语音打断”的上下文标注保留，转写完成后再开始下一轮。

“快捷键”设置页可分别录入语音输入、隐藏/显示桌宠、打开/关闭设置、打开/关闭聊天框四个
动作，支持单键，以及由 `Ctrl`、`Alt`、`Shift`、`Super` 中多个修饰键和一个主键组成的组合；
录入时按 `Esc` 可清空绑定。Windows、macOS 和 Linux X11 通过 `global-hotkey` 注册，原生
Wayland 通过 XDG GlobalShortcuts portal 请求合成器授权并接收按下、松开事件；portal 返回的
绑定子集才会被视为生效。

推理偏好默认使用 CPU，所有常规构建都会直接包含该平台的通用 GPU 后端：macOS 使用 Metal，
Windows 和 Linux 使用 Vulkan，不需要额外启用 Cargo feature。Vulkan 通过系统驱动覆盖
NVIDIA、AMD 和 Intel 设备。CUDA、ROCm 和 Intel SYCL 会直接链接供应商 SDK 及运行库，若将
它们加入同一个产物，没有对应运行库的机器会在应用启动前加载失败，因此不属于通用二进制。
Windows 和 Linux 运行环境仍需由 GPU 驱动或系统软件包提供 Vulkan loader。

每个本地 Whisper 模型可独立启用 GPU，并可指定目标语言；默认语言使用自动识别。Whisper 会
尝试使用已编译的 Metal 或 Vulkan 设备，模型初始化或推理失败时 LunaMate 自动重试 CPU。
whisper.cpp 1.8.3 当前仍强制 Silero VAD 使用 CPU，因此 GPU 开关只影响对应模型的 Whisper 转写。

### VAD 窗口与推理取消

自动模式从最终二进制内嵌的 `ggml-silero-v6.2.0.bin` 初始化，并只使用
`whisper-rs 0.16.0` 的公开安全 VAD API，不包含本地
`whisper-rs-sys` patch。LunaMate 每积累 256 ms 音频，就用最近约 1.024 秒的重叠窗口重新
推理，并只消费最新 256 ms 的概率。每次调用会从零状态开始，再由窗口中的历史音频预热
Silero LSTM；概率会与对应 PCM 帧重新对齐后再进入预录、迟滞和静音状态机。

`src/voice/transcribe.rs` 通过 whisper-rs 公开的 unsafe setter 安装 abort callback，让配置切换
和应用关闭可以中断同步 Whisper 推理；这条路径不需要 `raw-api` feature。没有使用 0.16.0 的
`FullParams::set_abort_callback_safe`，因为该版本的 callback trampoline 类型与 allocation
所有权尚不能满足可验证的生命周期。本地 user-data 指针只在同步
`whisper_full_with_state` 调用期间有效，且 callback 只读取线程安全的取消状态。whisper-rs
修复安全 callback 后，应删除这处本地 `unsafe` callback。

## 许可证

LunaMate 源代码采用 [MIT License](LICENSE)。第三方代码和素材继续遵循各自目录中记录的
许可证。
