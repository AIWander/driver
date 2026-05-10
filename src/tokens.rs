use crate::openai::Message;

pub fn estimate_tokens(s: &str) -> usize {
    (s.chars().count() as f32 / 3.5).ceil() as usize
}

pub fn estimate_history_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content = m.content.as_deref().unwrap_or("");
            let tool_calls = m
                .tool_calls
                .as_ref()
                .map(|tc| serde_json::to_string(tc).unwrap_or_default())
                .unwrap_or_default();
            estimate_tokens(content) + estimate_tokens(&tool_calls)
        })
        .sum()
}
