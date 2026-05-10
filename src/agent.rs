use anyhow::{bail, Result};
use serde_json::json;
use tracing::info;

use crate::config::{ModelConfig, RerankerConfig};
use crate::reranker;
use crate::tokens;
use crate::events::EventSink;
use crate::openai::{ChatCompletionRequest, Message, OpenAIClient, Usage};
use crate::registry::ToolRegistry;

pub async fn run_agent_loop(
    user_prompt: &str,
    system_prompt: &str,
    max_iterations: u32,
    model: &ModelConfig,
    client: &OpenAIClient,
    registry: &mut ToolRegistry,
    events: &mut dyn EventSink,
    reranker_cfg: Option<&RerankerConfig>,
) -> Result<AgentResult> {
    let tools_for_llm = registry.to_openai_tools();
    let tool_names: Vec<String> = tools_for_llm
        .iter()
        .map(|t| t.function.name.clone())
        .collect();

    events.log(
        "tools_registered",
        json!({"count": tool_names.len(), "names": tool_names}),
    )?;

    let mut messages: Vec<Message> = Vec::new();

    if model.system_prompt_strategy == "first_user_turn" {
        messages.push(Message {
            role: "user".to_string(),
            content: Some(format!("{}\n\n{}", system_prompt, user_prompt)),
            tool_calls: None,
            tool_call_id: None,
        });
    } else {
        messages.push(Message {
            role: "system".to_string(),
            content: Some(system_prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
        messages.push(Message {
            role: "user".to_string(),
            content: Some(user_prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    let start = std::time::Instant::now();
    let mut total_usage = Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    };
    let mut iterations = 0;

    loop {
        iterations += 1;
        if iterations > max_iterations {
            bail!("max iterations ({}) exceeded", max_iterations);
        }

        events.log(
            "llm_request",
            json!({
                "iteration": iterations,
                "model": model.model_id,
                "message_count": messages.len()
            }),
        )?;

        let request = ChatCompletionRequest {
            model: model.model_id.clone(),
            messages: messages.clone(),
            tools: if tools_for_llm.is_empty() {
                None
            } else {
                Some(tools_for_llm.clone())
            },
            tool_choice: if tools_for_llm.is_empty() {
                None
            } else {
                Some("auto".to_string())
            },
            max_tokens: Some(model.max_tokens),
            temperature: Some(model.temperature),
            stream: false,
        };

        let response = client.chat_completion(&request).await?;

        if let Some(usage) = &response.usage {
            total_usage.prompt_tokens += usage.prompt_tokens;
            total_usage.completion_tokens += usage.completion_tokens;
            total_usage.total_tokens += usage.total_tokens;
        }

        let choice = response
            .choices
            .first()
            .ok_or_else(|| anyhow::anyhow!("no choices in LLM response"))?;

        let content = choice.message.content.clone();
        let reasoning = choice
            .message
            .reasoning_content
            .clone()
            .or_else(|| choice.message.reasoning.clone());
        let tool_calls = choice.message.tool_calls.clone().unwrap_or_default();

        let display_content = if model.strip_thinking_tags {
            content.as_deref().map(strip_thinking_tags).map(String::from)
        } else {
            content.clone()
        };

        if let Some(ref r) = reasoning {
            events.log(
                "reasoning",
                json!({"iteration": iterations, "content": r}),
            )?;
        }

        if tool_calls.is_empty() {
            let final_content = display_content.unwrap_or_default();
            events.log(
                "final_answer",
                json!({"iteration": iterations, "content": final_content}),
            )?;

            let duration = start.elapsed();
            return Ok(AgentResult {
                final_answer: final_content,
                iterations,
                duration_ms: duration.as_millis() as u64,
                total_usage,
            });
        }

        // Push assistant message WITHOUT reasoning_content
        messages.push(Message {
            role: "assistant".to_string(),
            content: content.clone(),
            tool_calls: Some(tool_calls.clone()),
            tool_call_id: None,
        });

        for call in &tool_calls {
            info!(
                "iteration {}: calling {}",
                iterations, call.function.name
            );

            events.log(
                "tool_call",
                json!({
                    "iteration": iterations,
                    "id": call.id,
                    "name": call.function.name,
                    "arguments": call.function.arguments
                }),
            )?;

            let args: serde_json::Value =
                serde_json::from_str(&call.function.arguments).unwrap_or(json!({}));

            let result = registry.dispatch(&call.function.name, args).await;

            let (ok, raw_content) = match result {
                Ok(r) => (!r.is_error, r.text_content()),
                Err(e) => (false, format!("Error: {}", e)),
            };

            // Rerank large tool results via Qwen to compress context
            let final_content = if ok {
                if let Some(rcfg) = reranker_cfg {
                    if rcfg.enabled && raw_content.len() >= rcfg.min_tool_result_bytes {
                        match reranker::rerank_tool_result(
                            user_prompt,
                            &call.function.name,
                            &call.function.arguments,
                            &raw_content,
                            rcfg,
                        )
                        .await
                        {
                            Ok(Some(compressed)) => {
                                let saved = raw_content.len().saturating_sub(compressed.len());
                                events.log(
                                    "rerank",
                                    json!({
                                        "iteration": iterations,
                                        "tool": call.function.name,
                                        "raw_bytes": raw_content.len(),
                                        "compressed_bytes": compressed.len(),
                                        "saved_bytes": saved,
                                    }),
                                )?;
                                compressed
                            }
                            Ok(None) => raw_content.clone(),
                            Err(e) => {
                                tracing::warn!("rerank failed for {}: {}", call.function.name, e);
                                raw_content.clone()
                            }
                        }
                    } else {
                        raw_content.clone()
                    }
                } else {
                    raw_content.clone()
                }
            } else {
                raw_content.clone()
            };

            events.log(
                "tool_result",
                json!({
                    "iteration": iterations,
                    "id": call.id,
                    "ok": ok,
                    "content": final_content
                }),
            )?;

            messages.push(Message {
                role: "tool".to_string(),
                content: Some(final_content),
                tool_calls: None,
                tool_call_id: Some(call.id.clone()),
            });
        }

        // Auto-handoff: compact context if approaching model window limit
        let est = tokens::estimate_history_tokens(&messages);
        let trigger = (model.context_window as f32 * model.compaction_threshold) as usize;
        if est > trigger {
            let run_ts = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
            match compact_history(&messages, model, client, events, &run_ts, iterations).await {
                Ok(summary) => {
                    let tokens_after;
                    let summary_chars = summary.chars().count();
                    messages = vec![
                        messages[0].clone(), // preserve original system prompt
                        Message {
                            role: "user".to_string(),
                            content: Some(format!(
                                "[CONTINUING from auto-handoff. Original goal preserved below — pick up exactly where the previous loop left off.]

{}",
                                summary
                            )),
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    ];
                    tokens_after = tokens::estimate_history_tokens(&messages);
                    events.log("compaction", json!({
                        "iteration": iterations,
                        "tokens_before": est,
                        "tokens_after": tokens_after,
                        "summary_chars": summary_chars,
                    }))?;
                    info!(
                        "compaction at iteration {}: {} -> {} tokens",
                        iterations, est, tokens_after
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "compaction failed at iteration {}: {} — continuing with full history",
                        iterations, e
                    );
                }
            }
        }
    }
}


async fn compact_history(
    messages: &[Message],
    model: &ModelConfig,
    client: &OpenAIClient,
    sink: &mut dyn EventSink,
    run_id: &str,
    iteration: u32,
) -> Result<String> {
    let compaction_instruction = Message {
        role: "user".to_string(),
        content: Some(
            "Before we continue, summarize the conversation so far as a structured handoff. Use this exact format:

## Original goal
<one sentence>

## Achieved
- <bullet>

## Remaining
- <bullet>

## Key facts learned
- <bullet>

## Active state
- Last URL: <url or N/A>
- Last successful tool: <tool>
- Pending question: <if any>

## Next action
<one sentence>

Return ONLY the summary in that format. Do NOT call any tools. Do NOT respond conversationally."
                .to_string(),
        ),
        tool_calls: None,
        tool_call_id: None,
    };

    let mut compact_messages = messages.to_vec();
    compact_messages.push(compaction_instruction);

    let req = ChatCompletionRequest {
        model: model.model_id.clone(),
        messages: compact_messages,
        tools: None,
        tool_choice: None,
        max_tokens: Some(2048),
        temperature: Some(0.3),
        stream: false,
    };

    sink.log(
        "llm_request",
        json!({
            "iteration": iteration,
            "model": model.model_id,
            "purpose": "compaction_summary"
        }),
    )?;

    let resp = client.chat_completion(&req).await?;
    let summary = resp
        .choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .cloned()
        .unwrap_or_else(|| "(compaction returned empty)".to_string());

    let handoff_dir = std::path::Path::new("/opt/cpc/handoffs/agent_loop");
    let _ = std::fs::create_dir_all(handoff_dir);
    let handoff_path = handoff_dir.join(format!("{}_iter{}.md", run_id, iteration));
    let _ = std::fs::write(&handoff_path, &summary);

    Ok(summary)
}

fn strip_thinking_tags(content: &str) -> &str {
    lazy_static_regex(content)
}

fn lazy_static_regex(content: &str) -> &str {
    // Strip <|channel|>...<|end|> thinking blocks
    // Simple approach: if the entire content starts with <|channel|>, strip it
    if let Some(end_pos) = content.find("<|end|>") {
        if content.starts_with("<|channel|>") {
            let after = &content[end_pos + 7..];
            return after.trim();
        }
    }
    content
}

pub struct AgentResult {
    #[allow(dead_code)]
    pub final_answer: String,
    pub iterations: u32,
    pub duration_ms: u64,
    pub total_usage: Usage,
}
