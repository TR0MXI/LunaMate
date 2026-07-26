# Lobe Icons — LLM provider marks

The SVG files in this directory are derived from the
[Lobe Icons](https://github.com/lobehub/lobe-icons) collection.

- Commit: `f07e9be35aef452ce735f95ea8204a14ecc513f7`
- Source: `packages/react-native/src/icons/<Component>/components/Mono.tsx`
- License: MIT (see `LICENSE`), Copyright (c) 2023 LobeHub

Only the single-colour `Mono` variants are used. Each React Native component was
mechanically converted into a standalone SVG: the JSX element names were
lower-cased, `fillRule`/`clipRule` were renamed to their SVG attribute spelling,
and the `{color}` expression was replaced with an explicit opaque black fill
because GPUI rasterises SVG assets into an alpha mask and tints them with the
element text colour.

File names are LunaMate's stable `LlmProvider::id()` values, so the renderer can
resolve an icon without another lookup table.

| File | Upstream component |
| --- | --- |
| `aihubmix.svg` | `AiHubMix` |
| `aliyun.svg` | `AlibabaCloud` |
| `anthropic.svg` | `Anthropic` |
| `baidu.svg` | `BaiduCloud` |
| `bedrock-api-key.svg` | `Bedrock` |
| `bigmodel.svg` | `Zhipu` |
| `cohere.svg` | `Cohere` |
| `deepseek.svg` | `DeepSeek` |
| `fireworks.svg` | `Fireworks` |
| `gemini.svg` | `Gemini` |
| `github-models.svg` | `Github` |
| `groq.svg` | `Groq` |
| `mimo.svg` | `XiaomiMiMo` |
| `minimax.svg` | `Minimax` |
| `moonshot.svg` | `Moonshot` |
| `nebius.svg` | `Nebius` |
| `ollama.svg` | `Ollama` |
| `ollama-cloud.svg` | `Ollama` |
| `openai.svg` | `OpenAI` |
| `openai-responses.svg` | `OpenAI` |
| `opencode-go.svg` | `OpenCode` |
| `openrouter.svg` | `OpenRouter` |
| `together.svg` | `Together` |
| `vertex.svg` | `VertexAI` |
| `xai.svg` | `XAI` |
| `zai.svg` | `ZAI` |

## Trademarks

The MIT licence covers the icon artwork as distributed by LobeHub. The marks
themselves remain the trademarks of their respective owners. LunaMate displays
them only to identify which provider a connection entry targets; this implies no
affiliation with or endorsement by those owners.
