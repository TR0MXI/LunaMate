//! 维护模型能力、Provider 与思考强度的稳定配置标识。

use super::{LlmProvider, ReasoningEffort};

/// LunaMate 配置和界面按此顺序支持 Provider；类型本身直接复用 `genai` adapter 标识。
pub const LLM_PROVIDERS: [LlmProvider; 26] = [
    LlmProvider::OpenAI,
    LlmProvider::OpenAIResp,
    LlmProvider::Gemini,
    LlmProvider::Anthropic,
    LlmProvider::Fireworks,
    LlmProvider::Together,
    LlmProvider::Groq,
    LlmProvider::Aihubmix,
    LlmProvider::Mimo,
    LlmProvider::Moonshot,
    LlmProvider::Nebius,
    LlmProvider::Xai,
    LlmProvider::DeepSeek,
    LlmProvider::Zai,
    LlmProvider::BigModel,
    LlmProvider::Aliyun,
    LlmProvider::Baidu,
    LlmProvider::Cohere,
    LlmProvider::Ollama,
    LlmProvider::OllamaCloud,
    LlmProvider::Vertex,
    LlmProvider::GithubCopilot,
    LlmProvider::OpenCodeGo,
    LlmProvider::BedrockApi,
    LlmProvider::OpenRouter,
    LlmProvider::MiniMax,
];

/// 模型条目提供的专业能力；稳定 ID 同时用于配置文件与 UI 分组。
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ModelKind {
    #[default]
    ChatCompletions,
    SpeechSynthesis,
    Transcription,
}

impl ModelKind {
    pub const fn id(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat-completions",
            Self::SpeechSynthesis => "speech-synthesis",
            Self::Transcription => "transcription",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "chat-completions" => Some(Self::ChatCompletions),
            "speech-synthesis" => Some(Self::SpeechSynthesis),
            "transcription" => Some(Self::Transcription),
            _ => None,
        }
    }
}

/// 一个模型条目的执行后端。远端对话继续直接复用 `genai` 的 Provider 标识。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModelProvider {
    Genai(LlmProvider),
    Doubao,
    LocalWhisper,
}

impl Default for ModelProvider {
    fn default() -> Self {
        Self::Genai(LlmProvider::Ollama)
    }
}

impl From<LlmProvider> for ModelProvider {
    fn from(provider: LlmProvider) -> Self {
        Self::Genai(provider)
    }
}

impl ModelProvider {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Genai(provider) => llm_provider_id(provider),
            Self::Doubao => "doubao",
            Self::LocalWhisper => "local-whisper",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "doubao" => Some(Self::Doubao),
            "local-whisper" => Some(Self::LocalWhisper),
            _ => llm_provider_from_id(id).map(Self::Genai),
        }
    }

    pub const fn genai(self) -> Option<LlmProvider> {
        match self {
            Self::Genai(provider) => Some(provider),
            Self::Doubao | Self::LocalWhisper => None,
        }
    }
}

/// 返回写入 LunaMate 配置文件和资源路径的稳定 Provider ID。
pub const fn llm_provider_id(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::OpenAI => "openai",
        LlmProvider::OpenAIResp => "openai-responses",
        LlmProvider::Gemini => "gemini",
        LlmProvider::Anthropic => "anthropic",
        LlmProvider::Fireworks => "fireworks",
        LlmProvider::Together => "together",
        LlmProvider::Groq => "groq",
        LlmProvider::Aihubmix => "aihubmix",
        LlmProvider::Mimo => "mimo",
        LlmProvider::Moonshot => "moonshot",
        LlmProvider::Nebius => "nebius",
        LlmProvider::Xai => "xai",
        LlmProvider::DeepSeek => "deepseek",
        LlmProvider::Zai => "zai",
        LlmProvider::BigModel => "bigmodel",
        LlmProvider::Aliyun => "aliyun",
        LlmProvider::Baidu => "baidu",
        LlmProvider::Cohere => "cohere",
        LlmProvider::Ollama => "ollama",
        LlmProvider::OllamaCloud => "ollama-cloud",
        LlmProvider::Vertex => "vertex",
        LlmProvider::GithubCopilot => "github-models",
        LlmProvider::OpenCodeGo => "opencode-go",
        LlmProvider::BedrockApi => "bedrock-api-key",
        LlmProvider::OpenRouter => "openrouter",
        LlmProvider::MiniMax => "minimax",
    }
}

/// 从 LunaMate 的稳定配置 ID 恢复 `genai` adapter 标识。
pub fn llm_provider_from_id(id: &str) -> Option<LlmProvider> {
    LLM_PROVIDERS
        .into_iter()
        .find(|provider| llm_provider_id(*provider) == id)
}

pub const REASONING_EFFORT_LEVELS: [ReasoningEffort; 7] = [
    ReasoningEffort::None,
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];

/// 返回写入 LunaMate 配置文件的稳定思考强度 ID。
pub fn reasoning_effort_id(effort: &ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "off",
        _ => effort.variant_name(),
    }
}

/// 从 LunaMate 的稳定配置 ID 与可选预算恢复思考强度。
pub fn reasoning_effort_from_id(id: &str, budget: Option<u32>) -> Option<ReasoningEffort> {
    if id == "off" {
        return Some(ReasoningEffort::None);
    }
    if id == "budget" {
        return Some(ReasoningEffort::Budget(
            budget.unwrap_or(super::DEFAULT_REASONING_BUDGET),
        ));
    }
    REASONING_EFFORT_LEVELS
        .iter()
        .skip(1)
        .find(|effort| effort.variant_name() == id)
        .cloned()
}

/// 返回自定义思考预算；固定强度没有预算值。
pub const fn reasoning_budget(effort: &ReasoningEffort) -> Option<u32> {
    match effort {
        ReasoningEffort::Budget(tokens) => Some(*tokens),
        _ => None,
    }
}

/// whisper.cpp 当前支持的目标语言代码；空值表示让模型自动识别语种。
pub const WHISPER_LANGUAGE_CODES: [&str; 100] = [
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su", "yue",
];

/// 与 [`WHISPER_LANGUAGE_CODES`] 顺序对应的英文名称，仅用于设置界面展示。
pub const WHISPER_LANGUAGE_NAMES: [&str; 100] = [
    "English",
    "Chinese",
    "German",
    "Spanish",
    "Russian",
    "Korean",
    "French",
    "Japanese",
    "Portuguese",
    "Turkish",
    "Polish",
    "Catalan",
    "Dutch",
    "Arabic",
    "Swedish",
    "Italian",
    "Indonesian",
    "Hindi",
    "Finnish",
    "Vietnamese",
    "Hebrew",
    "Ukrainian",
    "Greek",
    "Malay",
    "Czech",
    "Romanian",
    "Danish",
    "Hungarian",
    "Tamil",
    "Norwegian",
    "Thai",
    "Urdu",
    "Croatian",
    "Bulgarian",
    "Lithuanian",
    "Latin",
    "Maori",
    "Malayalam",
    "Welsh",
    "Slovak",
    "Telugu",
    "Persian",
    "Latvian",
    "Bengali",
    "Serbian",
    "Azerbaijani",
    "Slovenian",
    "Kannada",
    "Estonian",
    "Macedonian",
    "Breton",
    "Basque",
    "Icelandic",
    "Armenian",
    "Nepali",
    "Mongolian",
    "Bosnian",
    "Kazakh",
    "Albanian",
    "Swahili",
    "Galician",
    "Marathi",
    "Punjabi",
    "Sinhala",
    "Khmer",
    "Shona",
    "Yoruba",
    "Somali",
    "Afrikaans",
    "Occitan",
    "Georgian",
    "Belarusian",
    "Tajik",
    "Sindhi",
    "Gujarati",
    "Amharic",
    "Yiddish",
    "Lao",
    "Uzbek",
    "Faroese",
    "Haitian Creole",
    "Pashto",
    "Turkmen",
    "Nynorsk",
    "Maltese",
    "Sanskrit",
    "Luxembourgish",
    "Myanmar",
    "Tibetan",
    "Tagalog",
    "Malagasy",
    "Assamese",
    "Tatar",
    "Hawaiian",
    "Lingala",
    "Hausa",
    "Bashkir",
    "Javanese",
    "Sundanese",
    "Cantonese",
];
