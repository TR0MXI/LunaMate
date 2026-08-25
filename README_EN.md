# LunaMate

English | [简体中文](https://github.com/TR0MXI/LunaMate/blob/master/README.md)

LunaMate is a cross-platform Live2D desktop companion built with Rust, with model interactions and
local or cloud AI chat.

> [!WARNING]
> LunaMate is a rapid prototype and has not published a release. Until the first public release,
> configuration formats, the database, session snapshots, and internal APIs may change without
> migration or backward compatibility. Source updates may require deleting the local
> `config.toml` and `data/lunamate.db` and configuring the application again.

The project is under active development and currently includes:

- Live2D rendering, eye tracking, and model interactions.
- Desktop support for Windows, macOS, and Linux.
- Configurable local or cloud AI chat, image input, and session saving.
- A screenshot tool that is disabled by default and requires explicit permission.
- Settings for models, appearance, windows, languages, shortcuts, and AI connections.

## Build

### General requirements

All platforms require the latest stable Rust toolchain, CMake, and Git. Install Rust through
[`rustup`](https://www.rust-lang.org/tools/install); it provides both `rustc` and `cargo`.

### Windows

1. Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
   with the “Desktop development with C++” workload, MSVC tools, and the Windows SDK.
2. Install [CMake](https://cmake.org/download/) and the
   [Vulkan SDK](https://vulkan.lunarg.com/sdk/home/). Reopen the terminal after installation.
3. Confirm that Rust uses the MSVC toolchain in PowerShell:

```powershell
rustup default stable-x86_64-pc-windows-msvc
```

### macOS

1. Install Xcode Command Line Tools:

```bash
xcode-select --install
```

2. Install [Homebrew](https://brew.sh/), then install CMake and `pkg-config`:

```bash
brew install cmake pkg-config
```

### Linux

The following commands apply to Ubuntu or Debian. Other distributions should install the equivalent
C/C++ toolchain, CMake, `pkg-config`, ALSA, Wayland, Vulkan development packages, and `glslc`.

```bash
sudo apt update
sudo apt install build-essential cmake pkg-config libasound2-dev \
  libfontconfig1-dev libwayland-dev libxkbcommon-dev \
  libvulkan-dev glslc
```

For Linux desktop portal features, also install `xdg-desktop-portal` and the backend for the desktop
environment, such as `xdg-desktop-portal-gnome` or `xdg-desktop-portal-kde`. The system may ask for
permission when a related feature is used. Linux packages should also install
`assets/linux/io.github.tr0mxi.lunamate.desktop` into a user or system applications directory.

### Build and run

From the repository root, create a development build:

```bash
cargo build --locked
```

Create an optimized build or run it directly:

```bash
cargo build --release --locked
cargo run --release --locked
```

The optimized executable is written to `target/release/lunamate`, or
`target/release/lunamate.exe` on Windows.

Live2D models are not distributed with this repository. Place a properly licensed model that
contains a `.model3.json` manifest under `models/`, then select it in the settings window.

## License

LunaMate source code is available under the [MIT License](LICENSE). Third-party code and assets
remain subject to the licenses recorded in their respective directories.
