use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    #[serde(default)]
    pub content: Vec<ContentPart>,
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    #[serde(default)]
    pub text: Option<String>,
}

impl ToolCallResult {
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| {
                if p.part_type == "text" {
                    p.text.as_deref()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn text_result(text: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentPart {
            part_type: "text".to_string(),
            text: Some(text.into()),
        }],
        is_error: false,
    }
}

pub fn error_result(text: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentPart {
            part_type: "text".to_string(),
            text: Some(text.into()),
        }],
        is_error: true,
    }
}

// --- JSON-RPC types ---

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
}

pub struct McpClient {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    pub fn spawn(command: &str, args: &[String], env: &HashMap<String, String>) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning MCP server: {}", command))?;

        let stdin = child.stdin.take().context("no stdin on child")?;
        let stdout = child.stdout.take().context("no stdout on child")?;

        Ok(McpClient {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
        })
    }

    async fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        let body = serde_json::to_string(&req)?;
        self.stdin.write_all(body.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;

        self.read_response().await
    }

    async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        #[derive(Serialize)]
        struct JsonRpcNotification {
            jsonrpc: &'static str,
            method: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            params: Option<Value>,
        }

        let notif = JsonRpcNotification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        };

        let body = serde_json::to_string(&notif)?;
        self.stdin.write_all(body.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn read_response(&mut self) -> Result<Value> {
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await?;
            if n == 0 {
                bail!("MCP server closed stdout unexpectedly");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let body: Value = if trimmed.starts_with('{') {
                serde_json::from_str(trimmed).context("parsing bare JSON-RPC response")?
            } else if trimmed.starts_with("Content-Length:") {
                let len: usize = trimmed
                    .strip_prefix("Content-Length:")
                    .unwrap()
                    .trim()
                    .parse()
                    .context("bad Content-Length")?;
                loop {
                    let mut hdr = String::new();
                    self.reader.read_line(&mut hdr).await?;
                    if hdr.trim().is_empty() {
                        break;
                    }
                }
                let mut buf = vec![0u8; len];
                tokio::io::AsyncReadExt::read_exact(&mut self.reader, &mut buf).await?;
                serde_json::from_slice(&buf)?
            } else {
                continue;
            };

            if body.get("id").is_none() || body.get("id") == Some(&Value::Null) {
                continue;
            }

            let resp: JsonRpcResponse = serde_json::from_value(body)?;
            if let Some(err) = resp.error {
                bail!("MCP error {}: {}", err.code, err.message);
            }
            return resp
                .result
                .context("MCP response missing both result and error");
        }
    }

    pub async fn initialize(&mut self) -> Result<Value> {
        let params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "driver",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let result = self.request("initialize", Some(params)).await?;
        self.notify("notifications/initialized", None).await?;
        Ok(result)
    }

    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        let result = self
            .request("tools/list", Some(serde_json::json!({})))
            .await?;
        let tools: Vec<McpTool> =
            serde_json::from_value(result.get("tools").cloned().unwrap_or(Value::Array(vec![])))?;
        Ok(tools)
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<ToolCallResult> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments
        });
        let result = self.request("tools/call", Some(params)).await?;
        let call_result: ToolCallResult = serde_json::from_value(result)?;
        Ok(call_result)
    }

    pub async fn shutdown(mut self) -> Result<()> {
        drop(self.stdin);
        let _ = self.child.wait().await;
        Ok(())
    }
}
