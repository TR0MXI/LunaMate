# LunaMate

[English](https://github.com/TR0MXI/LunaMate/blob/master/README_EN.md) | 简体中文

LunaMate 是一款使用 Rust 构建的跨平台 Live2D 桌面宠物，支持模型互动和本地或云端 AI
对话。

> [!WARNING]
> LunaMate 尚处于快速原型阶段，尚未发布任何版本。首个公开版本前，配置格式、数据库、
> 会话快照和内部接口可能直接发生破坏性变更，不提供旧数据迁移或向后兼容保证。更新源码后，
> 可能需要删除本地 `config.toml` 和 `data/lunamate.db` 并重新配置。

项目仍在开发中，目前包含以下能力：

- Live2D 模型渲染、视线跟随，以及模型互动。
- Windows、macOS 和 Linux 桌面环境支持。
- 可配置的本地或云端 AI 对话、图片输入和会话保存。
- 默认关闭且需要用户显式授权的屏幕截图工具。
- 模型、外观、窗口、语言、快捷键和 AI 设置界面。

## 构建

### 通用准备

所有平台都需要安装最新 stable Rust、CMake 和 Git。推荐通过
[rustup](https://www.rust-lang.org/tools/install) 安装 Rust，安装后会同时获得 `rustc` 和
`cargo`。

### Windows

1. 安装 [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)，
   勾选“使用 C++ 的桌面开发”、MSVC 工具集和 Windows SDK。
2. 安装 [CMake](https://cmake.org/download/) 与
   [Vulkan SDK](https://vulkan.lunarg.com/sdk/home/)。安装完成后重新打开终端，使工具链环境变量生效。
3. 在 PowerShell 中确认 Rust 使用 MSVC 工具链：

```powershell
rustup default stable-x86_64-pc-windows-msvc
```

### macOS

1. 安装 Xcode Command Line Tools：

```bash
xcode-select --install
```

2. 安装 [Homebrew](https://brew.sh/)，然后安装 CMake 和 `pkg-config`：

```bash
brew install cmake pkg-config
```

### Linux

以下命令适用于 Ubuntu 或 Debian。其他发行版请安装对应的软件包：C/C++ 编译器、CMake、
`pkg-config`、ALSA、Wayland、Vulkan 开发库和 `glslc`。

```bash
sudo apt update
sudo apt install build-essential cmake pkg-config libasound2-dev \
  libfontconfig1-dev libwayland-dev libxkbcommon-dev \
  libvulkan-dev glslc
```

需要使用 Linux 桌面门户功能时，还需安装 `xdg-desktop-portal` 及桌面环境对应的后端，例如
`xdg-desktop-portal-gnome` 或 `xdg-desktop-portal-kde`。首次使用相关功能时，系统可能会请求授权。
Linux 发布包还应将 `assets/linux/io.github.tr0mxi.lunamate.desktop` 安装到用户或系统的
applications 目录。

### 编译运行

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
