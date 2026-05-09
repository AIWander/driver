use anyhow::{Context, Result};
use serde_json::json;
use std::path::PathBuf;
use tracing::info;

use crate::agent::{self, AgentResult};
use crate::config::{DriverConfig, RunRequest};
use crate::events::EventSink;
use crate::mcp;
use crate::openai;
use crate::registry::ToolRegistry;

pub async fn run_task(
    driver_config: &DriverConfig,
    req: &RunRequest,
    run_id: &str,
    sink: &mut dyn EventSink,
) -> Result<AgentResult> {
    let model = driver_config.find_model(&req.model)?;
    let task_name = req.name.as_deref().unwrap_or("unnamed");

    info!("run {}: model={} task={}", run_id, model.name, task_name);

    // Determine which MCP servers to use
    let default_servers = req
        .mcp_servers
        .as_ref()
        .unwrap_or(&model.mcp_servers);
    let tool_filter_list = req.tool_filter.as_ref().unwrap_or(&model.tool_filter);

    let system_prompt = req
        .system_prompt
        .clone()
        .unwrap_or_else(|| load_default_system_prompt());

    sink.log(
        "run_start",
        json!({
            "run_id": run_id,
            "name": task_name,
            "model": model.name,
            "base_url": model.base_url,
            "user_prompt": req.user_prompt,
            "mcp_servers": default_servers,
            "max_iterations": req.max_iterations,
        }),
    )?;

    // Build tool registry with driver's built-in tools
    let mut reg = ToolRegistry::new(&driver_config.server.log_dir);
    reg.register_driver_tools();

    // Spawn each MCP server
    for server_name in default_servers {
        let server_config = driver_config.find_server(server_name)?;
        info!(
            "spawning MCP server: {} ({})",
            server_name, server_config.command
        );

        let mut client = mcp::McpClient::spawn(
            &server_config.command,
            &server_config.args,
            &server_config.env,
        )?;

        let init_result = client
            .initialize()
            .await
            .with_context(|| format!("MCP initialize handshake with '{}'", server_name))?;
        info!(
            "initialized {}: {:?}",
            server_name,
            init_result.get("serverInfo")
        );

        let all_tools = client
            .list_tools()
            .await
            .with_context(|| format!("tools/list from '{}'", server_name))?;
        info!("{} exposes {} tools", server_name, all_tools.len());

        reg.add_server(server_name.clone(), client, all_tools, tool_filter_list)
            .await?;
    }

    info!("{} total tools registered", reg.tool_count());

    // Build OpenAI client
    let api_key = model
        .api_key_env
        .as_ref()
        .and_then(|env_name| std::env::var(env_name).ok());
    let openai_client = openai::OpenAIClient::new(&model.base_url, api_key);

    // Run agent loop
    let result = agent::run_agent_loop(
        &req.user_prompt,
        &system_prompt,
        req.max_iterations,
        model,
        &openai_client,
        &mut reg,
        sink,
    )
    .await;

    match &result {
        Ok(r) => {
            sink.log(
                "run_end",
                json!({
                    "ok": true,
                    "duration_ms": r.duration_ms,
                    "total_tokens": r.total_usage.total_tokens,
                    "iterations": r.iterations,
                }),
            )?;
        }
        Err(e) => {
            sink.log("run_end", json!({"ok": false, "error": e.to_string()}))?;
        }
    }

    // Shutdown MCP servers
    reg.shutdown_all().await?;

    result
}

fn load_default_system_prompt() -> String {
    let path = PathBuf::from("config/prompts/system.txt");
    std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "You are an agent with access to tools. Use the tools to accomplish the user's task. Provide a clear final answer when done.".to_string()
    })
}
