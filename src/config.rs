use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct DriverConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default = "default_log_dir")]
    pub log_dir: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            http_port: default_http_port(),
            log_dir: default_log_dir(),
        }
    }
}

fn default_http_port() -> u16 {
    8009
}
fn default_log_dir() -> String {
    "/opt/cpc/state".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    pub name: String,
    pub base_url: String,
    pub model_id: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub strip_thinking_tags: bool,
    #[serde(default = "default_system_prompt_strategy")]
    pub system_prompt_strategy: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub tool_filter: Vec<String>,
    #[serde(default)]
    pub tool_call_parser: Option<String>,
    #[serde(default)]
    pub reasoning_parser: Option<String>,
    #[serde(default = "default_auto_tool_choice")]
    pub auto_tool_choice: bool,
    #[serde(default)]
    pub system_prompt_files: Vec<String>,
}

fn default_system_prompt_strategy() -> String {
    "system_role".to_string()
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_temperature() -> f32 {
    0.7
}
fn default_auto_tool_choice() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunRequest {
    #[serde(default)]
    pub name: Option<String>,
    pub model: String,
    #[serde(default)]
    pub mcp_servers: Option<Vec<String>>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    #[serde(default)]
    pub tool_filter: Option<Vec<String>>,
}

fn default_max_iterations() -> u32 {
    30
}

pub fn load_config(path: &Path) -> Result<DriverConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading config: {}", path.display()))?;
    let config: DriverConfig =
        toml::from_str(&content).with_context(|| format!("parsing config: {}", path.display()))?;
    Ok(config)
}

impl DriverConfig {
    pub fn find_model(&self, name: &str) -> Result<&ModelConfig> {
        self.models
            .iter()
            .find(|m| m.name == name)
            .with_context(|| format!("model '{}' not found in config", name))
    }

    pub fn find_server(&self, name: &str) -> Result<&McpServerConfig> {
        self.mcp_servers
            .iter()
            .find(|s| s.name == name)
            .with_context(|| format!("mcp_server '{}' not found in config", name))
    }
}
