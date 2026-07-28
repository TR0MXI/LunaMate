# LunaMate

English | [简体中文](https://github.com/TR0MXI/LunaMate/blob/master/README.md)

LunaMate is a cross-platform Live2D desktop companion built with Rust. It uses GPUI for the
desktop window and settings UI, Mocari for Cubism model playback, and genai for local or cloud
language model integrations.

The project is under active development and currently includes:

- Live2D rendering, motions, expressions, eye tracking, and Agent interactions triggered by
  named HitAreas.
- Independent GPU underlays on Windows, macOS, and Wayland, with a CPU fallback on X11 or when
  GPU initialization is unavailable.
- Streaming multimodal LLM chat with configurable providers, models, and endpoints, image input,
  and bounded session persistence.
- Local voice input, automatic endpoint detection, and conversation interruption built on CPAL,
  Silero VAD, and whisper.cpp.
- An Agent screenshot tool that is disabled by default and requires explicit permission in Tool
  Settings.
- Settings for models, appearance, windows, languages, global shortcuts, LLM connections, and tools.

## Build

Install the latest stable Rust toolchain from the
[official Rust installation page](https://www.rust-lang.org/tools/install). Using `rustup` is
recommended; it installs both `rustc` and `cargo`.

You also need CMake and the native toolchain for your platform: MSVC Build Tools, the Windows SDK,
and the Vulkan SDK on Windows; Xcode Command Line Tools on macOS; or a C/C++ toolchain,
`pkg-config`, ALSA development files, and the Wayland, X11, and Vulkan development files plus
`glslc` on Linux.

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

### External Motions, Expressions, and Parameter Outfits

In addition to resources declared by `.model3.json`, LunaMate checks direct files beside the
manifest and direct files in the following dedicated directories. It does not recurse below them:

- `*.motion3.json` files beside the manifest or under `motions/` are loaded as individual motions.
  Cubism 3 motions recorded by VTube Studio can be placed there directly. External motions play
  once for preview and then return to `Idle`.
- `*.exp3.json` files beside the manifest or under `expressions/` are loaded as expressions. Files
  under `expressions/` stay in the expression section. Files beside the manifest start there but
  can be dragged into the outfit section for VTube Studio-style parameter outfits, or dragged back.

Choose **Rescan** on the Model Settings page after adding files. Motions, expressions, parameter
outfits, and complete `.model3.json` outfit variants can all be renamed there. Aliases are stored
only in LunaMate's `config.toml`; model files and filenames are never changed. Outfit aliases are
also published to the Agent's `change_outfit` tool. Playback continues to use stable internal IDs,
so aliases do not change the actual target. Motions and expressions remain model-specific: a file
using parameter IDs from another model may parse successfully without producing the expected look.

## Voice Input

Voice input is disabled by default. Select a whisper.cpp GGML Whisper model and a compatible
Silero VAD GGML model on the Voice settings page before enabling it.

Automatic mode continuously listens and submits an utterance after VAD detects its endpoint. It
also lets the global Voice Input shortcut take over the current candidate or active recording;
releasing the shortcut submits the recording and restores automatic listening. Push-to-talk mode
records only while that shortcut is held.

The Shortcuts settings page configures Voice Input, Hide/Show Desktop Pet, Open/Close Settings,
and Open/Close Chat. A binding may use one main key plus any combination of `Ctrl`, `Alt`, `Shift`,
and `Super`; press `Esc` while recording to clear it. Windows, macOS, and Linux X11 register through
`global-hotkey`. Native Wayland uses the XDG GlobalShortcuts portal and treats only the compositor-
confirmed subset as active. Linux packages must install
`assets/linux/io.github.tr0mxi.lunamate.desktop` into an applications directory so the host portal
can identify LunaMate and restore approved bindings.

Every regular build includes the portable GPU backend for its platform without requiring a Cargo
feature: Metal on macOS and Vulkan on Windows and Linux. Vulkan reaches NVIDIA, AMD, and Intel
devices through their system drivers. The Windows and Linux runtime must provide a Vulkan loader,
normally installed with the GPU driver or system Vulkan package. CUDA, ROCm, and Intel SYCL are not
included because they link vendor SDK runtimes directly and would prevent one binary from running
on systems where those runtimes are absent.

When GPU acceleration is enabled in settings, Whisper first attempts Metal or Vulkan and falls
back to CPU after an initialization or inference failure. Silero VAD remains on CPU because
whisper.cpp 1.8.3 currently forces that backend.

## License

LunaMate source code is available under the [MIT License](LICENSE). Third-party code and assets
remain subject to the licenses recorded in their respective directories.
