use crate::mcp::McpTool;
use crate::openai::{OpenAIFunction, OpenAITool};

pub fn mcp_tool_to_openai(name: &str, tool: &McpTool) -> OpenAITool {
    OpenAITool {
        tool_type: "function".to_string(),
        function: OpenAIFunction {
            name: name.to_string(),
            description: tool.description.clone().unwrap_or_default(),
            parameters: tool.input_schema.clone(),
        },
    }
}
