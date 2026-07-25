# LunaMate

English | [简体中文](https://github.com/TR0MXI/LunaMate/blob/master/README.md)

LunaMate is a cross-platform Live2D desktop companion built with Rust. It uses GPUI for the
desktop window and settings UI, Mocari for Cubism model playback, and genai for local or cloud
language model integrations.

The project is under active development and currently includes:

- Live2D rendering, motions, expressions, eye tracking, and HitArea click reactions.
- Independent GPU underlays on Windows, macOS, and Wayland, with a CPU fallback on X11 or when
  GPU initialization is unavailable.
- Streaming multimodal LLM chat with configurable providers, models, and endpoints, image input,
  and bounded session persistence.
- An Agent screenshot tool that is disabled by default and requires explicit permission in Tool
  Settings.
- Settings for models, appearance, windows, languages, LLM connections, and tools.

## Build

Install the latest stable Rust toolchain from the
[official Rust installation page](https://www.rust-lang.org/tools/install). Using `rustup` is
recommended; it installs both `rustc` and `cargo`.

You also need the native toolchain for your platform: MSVC Build Tools and the Windows SDK on
Windows, Xcode Command Line Tools on macOS, or a C/C++ toolchain, `pkg-config`, and the Wayland,
X11, and Vulkan development libraries on Linux.

On Linux, the screenshot tool also requires `xdg-desktop-portal` and a portal backend supplied by
the desktop environment. The system may ask for screen access on first use.

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
