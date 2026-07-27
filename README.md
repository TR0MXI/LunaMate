# LunaMate

[English](https://github.com/TR0MXI/LunaMate/blob/master/README_EN.md) | 简体中文

LunaMate 是一款使用 Rust 构建的跨平台 Live2D 桌面宠物。项目以 GPUI 提供桌面窗口和
设置界面，以 Mocari 驱动 Cubism 模型，并通过 genai 接入本地或云端语言模型。

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

## 语音输入

语音设置默认关闭。LunaMate 不会自动下载语音模型；使用前需要在设置的“语音”页面选择：

- whisper.cpp GGML 格式的 Whisper 模型，用于本地语音转文字。
- whisper.cpp 支持的 Silero VAD GGML 模型，用于自动模式的人声起止检测。

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

设置中开启 GPU 后，Whisper 会尝试使用已编译的 Metal 或 Vulkan 设备；模型初始化或推理失败
时 LunaMate 自动重试 CPU。whisper.cpp 1.8.3 当前仍强制 Silero VAD 使用 CPU，因此 GPU 开关
只影响 Whisper 转写。

### VAD 窗口与 raw-api 例外

自动模式只使用 `whisper-rs 0.16.0` 的公开安全 VAD API，不包含本地
`whisper-rs-sys` patch。LunaMate 每积累 256 ms 音频，就用最近约 1.024 秒的重叠窗口重新
推理，并只消费最新 256 ms 的概率。每次调用会从零状态开始，再由窗口中的历史音频预热
Silero LSTM；概率会与对应 PCM 帧重新对齐后再进入预录、迟滞和静音状态机。

`raw-api` 当前仅用于 `src/voice/transcribe.rs` 的 abort callback，让配置切换和应用关闭可以
中断同步 Whisper 推理。没有使用 0.16.0 的 `FullParams::set_abort_callback_safe`，因为该版本
的 callback trampoline 类型与 allocation 所有权尚不能满足可验证的生命周期。本地
user-data 指针只在同步 `whisper_full_with_state` 调用期间有效，且 callback 只读取线程安全的
取消状态。whisper-rs 修复安全 callback 后，应删除 `raw-api` feature 和这处 `unsafe`。

## 许可证

LunaMate 源代码采用 [MIT License](LICENSE)。第三方代码和素材继续遵循各自目录中记录的
许可证。
