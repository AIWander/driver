use anyhow::Result;
use globset::GlobBuilder;
use serde_json::Value;
use std::collections::HashMap;
use tracing::info;

use crate::driver_tools::DriverTools;
use crate::mcp::{McpClient, McpTool, ToolCallResult};
use crate::openai::OpenAITool;
use crate::tools::mcp_tool_to_openai;

const DRIVER_SERVER: &str = "driver";

pub struct ToolRegistry {
    mcp_clients: HashMap<String, McpClient>,
    pub driver: DriverTools,
    tool_owners: HashMap<String, String>,
    raw_tools: HashMap<String, McpTool>,
}

impl ToolRegistry {
    pub fn new(log_dir: &str) -> Self {
        ToolRegistry {
            mcp_clients: HashMap::new(),
            driver: DriverTools::new(log_dir),
            tool_owners: HashMap::new(),
            raw_tools: HashMap::new(),
        }
    }

    pub fn register_driver_tools(&mut self) {
        let tools = DriverTools::tool_definitions();
        for tool in tools {
            self.register_tool(DRIVER_SERVER, tool);
        }
        info!(
            "driver registered {} built-in tools",
            self.tool_owners
                .values()
                .filter(|v| *v == DRIVER_SERVER)
                .count()
        );
    }

    fn register_tool(&mut self, server_name: &str, tool: McpTool) {
        let bare_name = tool.name.clone();

        if let Some(existing_server) = self.tool_owners.get(&bare_name).cloned() {
            let old_prefixed = format!("{}__{}", existing_server, bare_name);
            let new_prefixed = format!("{}__{}", server_name, bare_name);

            if let Some(old_tool) = self.raw_tools.remove(&bare_name) {
                self.tool_owners.remove(&bare_name);
                self.raw_tools.insert(old_prefixed.clone(), old_tool);
                self.tool_owners.insert(old_prefixed.clone(), existing_server.clone());
                info!(
                    "namespace collision on '{}': existing -> '{}'",
                    bare_name, old_prefixed
                );
            }

            self.raw_tools.insert(new_prefixed.clone(), tool);
            self.tool_owners.insert(new_prefixed.clone(), server_name.to_string());
            info!(
                "namespace collision on '{}': new -> '{}'",
                bare_name, new_prefixed
            );
        } else {
            self.tool_owners.insert(bare_name.clone(), server_name.to_string());
            self.raw_tools.insert(bare_name, tool);
        }
    }

    pub async fn add_server(
        &mut self,
        name: String,
        client: McpClient,
        all_tools: Vec<McpTool>,
        filter: &[String],
    ) -> Result<()> {
        let tools = if filter.is_empty() {
            all_tools
        } else {
            all_tools
                .into_iter()
                .filter(|t| {
                    filter.iter().any(|pattern| {
                        GlobBuilder::new(pattern)
                            .literal_separator(false)
                            .build()
                            .map(|g| g.compile_matcher().is_match(&t.name))
                            .unwrap_or(false)
                    })
                })
                .collect()
        };

        for tool in tools {
            self.register_tool(&name, tool);
        }

        self.mcp_clients.insert(name, client);
        Ok(())
    }

    pub fn to_openai_tools(&self) -> Vec<OpenAITool> {
        self.raw_tools
            .iter()
            .map(|(exposed_name, mcp_tool)| mcp_tool_to_openai(exposed_name, mcp_tool))
            .collect()
    }

    pub async fn dispatch(&mut self, tool_name: &str, arguments: Value) -> Result<ToolCallResult> {
        let server_name = self
            .tool_owners
            .get(tool_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {}", tool_name))?;

        let mcp_name = if tool_name.contains("__") {
            let prefix = format!("{}__", server_name);
            tool_name.strip_prefix(&prefix).unwrap_or(tool_name)
        } else {
            tool_name
        };

        if server_name == DRIVER_SERVER {
            self.driver.dispatch(mcp_name, arguments).await
        } else {
            let client = self
                .mcp_clients
                .get_mut(&server_name)
                .ok_or_else(|| anyhow::anyhow!("no client for server: {}", server_name))?;
            client.call_tool(mcp_name, arguments).await
        }
    }

    pub fn tool_count(&self) -> usize {
        self.raw_tools.len()
    }

    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.raw_tools.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn driver_tool_names(&self) -> Vec<String> {
        self.tool_owners
            .iter()
            .filter(|(_, v)| *v == DRIVER_SERVER)
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn mcp_server_tools(&self) -> HashMap<String, Vec<String>> {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for (tool_name, server_name) in &self.tool_owners {
            if server_name != DRIVER_SERVER {
                map.entry(server_name.clone())
                    .or_default()
                    .push(tool_name.clone());
            }
        }
        for tools in map.values_mut() {
            tools.sort();
        }
        map
    }

    pub async fn shutdown_all(self) -> Result<()> {
        for (name, client) in self.mcp_clients {
            if let Err(e) = client.shutdown().await {
                tracing::warn!("error shutting down {}: {}", name, e);
            }
        }
        Ok(())
    }
}
