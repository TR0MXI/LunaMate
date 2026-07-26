# LunaMate

[English](https://github.com/TR0MXI/LunaMate/blob/master/README_EN.md) | 简体中文

LunaMate 是一款使用 Rust 构建的跨平台 Live2D 桌面宠物。项目以 GPUI 提供桌面窗口和
设置界面，以 Mocari 驱动 Cubism 模型，并通过 genai 接入本地或云端语言模型。

项目仍在开发中，目前包含以下能力：

- Live2D 模型渲染、动作、表情、视线跟随，以及按 HitArea 部位触发的 Agent 互动。
- Windows、macOS 和 Wayland 独立 GPU underlay，以及 X11 或 GPU 不可用时的 CPU 回退。
- 可配置 Provider、模型和 endpoint 的流式多模态 LLM 对话、图片输入，以及有界会话持久化。
- 默认关闭且需要用户在 Tool 设置中显式授权的 Agent 屏幕截图工具。
- 模型、外观、窗口、语言、LLM 和 Tool 设置界面。

## 构建

先从 [Rust 官方安装页面](https://www.rust-lang.org/tools/install) 安装最新 stable Rust。
推荐使用 `rustup`，安装后会同时获得 `rustc` 和 `cargo`。

还需准备对应平台的原生工具链：Windows 使用 MSVC Build Tools 和 Windows SDK，macOS
使用 Xcode Command Line Tools，Linux 使用 C/C++ 工具链、`pkg-config` 以及 Wayland、
X11 和 Vulkan 开发库。

Linux 上使用截屏工具时，桌面环境还需提供 `xdg-desktop-portal` 及对应后端；首次使用可能
显示系统屏幕访问确认。

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

## 许可证

LunaMate 源代码采用 [MIT License](LICENSE)。第三方代码和素材继续遵循各自目录中记录的
许可证。
