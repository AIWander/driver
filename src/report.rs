use anyhow::Result;
use serde_json::Value;
use std::fs;
use std::io::BufRead;
use std::path::Path;

pub fn compose_report(run_dir: &Path) -> Result<String> {
    let jsonl_path = run_dir.join("run.jsonl");
    let file = fs::File::open(&jsonl_path)?;
    let reader = std::io::BufReader::new(file);

    let events: Vec<Value> = reader
        .lines()
        .filter_map(|line| line.ok())
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();

    let mut md = String::new();

    let run_start = events.iter().find(|e| e["kind"] == "run_start");
    let task_name = run_start
        .and_then(|e| e["name"].as_str().or(e["task"].as_str()))
        .unwrap_or("unnamed");
    let model_name = run_start
        .and_then(|e| e["model"].as_str())
        .unwrap_or("unknown");
    let timestamp = run_start
        .and_then(|e| e["ts"].as_str())
        .unwrap_or("unknown");
    let user_prompt = run_start
        .and_then(|e| e["user_prompt"].as_str())
        .unwrap_or("");

    let final_answer = events.iter().find(|e| e["kind"] == "final_answer");
    let answer_text = final_answer
        .and_then(|e| e["content"].as_str())
        .unwrap_or("(no answer)");

    let run_end = events.iter().find(|e| e["kind"] == "run_end");
    let duration_ms = run_end.and_then(|e| e["duration_ms"].as_u64()).unwrap_or(0);
    let total_tokens = run_end
        .and_then(|e| e["total_tokens"].as_u64())
        .unwrap_or(0);

    let max_iteration = events
        .iter()
        .filter_map(|e| e["iteration"].as_u64())
        .max()
        .unwrap_or(0);

    md.push_str(&format!("# Run: {} — {}\n\n", task_name, timestamp));
    md.push_str(&format!("**Model:** {} | **Duration:** {}ms | **Tokens:** {}\n\n", model_name, duration_ms, total_tokens));
    if !user_prompt.is_empty() {
        md.push_str(&format!("**Task:** {}\n\n", user_prompt));
    }
    md.push_str("---\n\n");

    md.push_str("## Final Answer\n\n");
    md.push_str(answer_text);
    md.push_str("\n\n---\n\n");

    md.push_str(&format!(
        "## Trace ({} iterations)\n\n",
        max_iteration
    ));

    for iter in 1..=max_iteration {
        md.push_str(&format!("### Iteration {}\n\n", iter));

        let reasoning_events: Vec<&Value> = events
            .iter()
            .filter(|e| e["kind"] == "reasoning" && e["iteration"].as_u64() == Some(iter))
            .collect();
        for r in &reasoning_events {
            if let Some(text) = r["content"].as_str() {
                let truncated = if text.len() > 500 {
                    format!("{}...", &text[..500])
                } else {
                    text.to_string()
                };
                md.push_str(&format!("*Reasoning:* {}\n\n", truncated));
            }
        }

        let calls: Vec<&Value> = events
            .iter()
            .filter(|e| e["kind"] == "tool_call" && e["iteration"].as_u64() == Some(iter))
            .collect();

        if calls.is_empty() {
            if let Some(fa) = events
                .iter()
                .find(|e| e["kind"] == "final_answer" && e["iteration"].as_u64() == Some(iter))
            {
                md.push_str(&format!(
                    "*Final answer:* {}\n\n",
                    fa["content"].as_str().unwrap_or("")
                ));
            }
        } else {
            for call in calls {
                let name = call["name"].as_str().unwrap_or("?");
                let call_id = call["id"].as_str().unwrap_or("");

                let result = events
                    .iter()
                    .find(|e| e["kind"] == "tool_result" && e["id"].as_str() == Some(call_id));
                let result_text = result
                    .and_then(|r| r["content"].as_str())
                    .unwrap_or("(no result)");
                let truncated = if result_text.len() > 200 {
                    format!("{}...", &result_text[..200])
                } else {
                    result_text.to_string()
                };

                md.push_str(&format!("- `{}` → `{}`\n", name, truncated));
            }
            md.push_str("\n");
        }
    }

    md.push_str("---\n");
    Ok(md)
}

pub fn write_report(run_dir: &Path) -> Result<()> {
    let report = compose_report(run_dir)?;
    fs::write(run_dir.join("shared_state.md"), report)?;
    Ok(())
}
