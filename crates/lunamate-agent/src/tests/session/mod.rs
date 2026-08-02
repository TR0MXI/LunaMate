use crate::{
    config::AppLanguage,
    memory::{AssistantTrace, ToolExecutionTrace},
};

const LANGUAGE: AppLanguage = AppLanguage::English;

fn reasoning_trace(reasoning: impl Into<String>) -> AssistantTrace {
    AssistantTrace::new(Some(reasoning.into()), Vec::new())
}

fn tool_trace(reasoning: &str) -> AssistantTrace {
    AssistantTrace::new(
        Some(reasoning.to_owned()),
        vec![ToolExecutionTrace::new(
            "local_tool".to_owned(),
            serde_json::json!({"input": reasoning}),
            serde_json::json!({"status": "ok"}),
        )],
    )
}

mod budget;
mod editing;
mod snapshot;
mod turns;
