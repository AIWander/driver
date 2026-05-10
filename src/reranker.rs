use anyhow::Result;
use tracing::warn;

use crate::config::RerankerConfig;
use crate::openai::{ChatCompletionRequest, Message, OpenAIClient};

pub async fn rerank_tool_result(
    user_prompt: &str,
    tool_name: &str,
    tool_args: &str,
    raw_result: &str,
    cfg: &RerankerConfig,
) -> Result<Option<String>> {
    if !cfg.enabled || raw_result.len() < cfg.min_tool_result_bytes {
        return Ok(None);
    }

    let client = OpenAIClient::new(&cfg.base_url, None);

    let system_msg = "You compress tool results for an AI agent. Keep ONLY the information relevant to the user's goal. Drop boilerplate, navigation, ads, repeated whitespace. Preserve numbers, prices, dates, names, URLs, and structured data exactly. Return at most 800 tokens of compressed content. No commentary — just the relevant content.".to_string();

    let user_msg = format!(
        "## User goal\n{}\n\n## Tool just called\n{}({})\n\n## Raw tool result ({} bytes)\n{}\n\n## Output\nReturn the relevant subset. Preserve exact numbers and quoted strings.",
        user_prompt, tool_name, tool_args, raw_result.len(), raw_result
    );

    let request = ChatCompletionRequest {
        model: cfg.model_id.clone(),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: Some(system_msg),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: "user".to_string(),
                content: Some(user_msg),
                tool_calls: None,
                tool_call_id: None,
            },
        ],
        tools: None,
        tool_choice: None,
        max_tokens: Some(cfg.max_summary_tokens),
        temperature: Some(0.2),
        stream: false,
    };

    let resp = match client.chat_completion(&request).await {
        Ok(r) => r,
        Err(e) => {
            warn!("reranker request failed: {}", e);
            return Ok(None);
        }
    };

    let compressed = resp
        .choices
        .first()
        .and_then(|c| c.message.content.clone());

    match compressed {
        Some(text) if !text.trim().is_empty() => Ok(Some(text)),
        _ => Ok(None),
    }
}
