mod agent;
mod breadcrumbs;
mod config;
mod driver_tools;
mod events;
mod loafs;
mod mcp;
mod openai;
mod policy;
mod registry;
mod report;
mod run;
mod serve;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "driver", about = "AIWander driver: consolidated MCP server + universal MCP-aware agent harness")]
struct Cli {
    #[arg(short, long, default_value = "config/driver.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start HTTP server (default mode)
    Serve {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
    },
    /// List tools from an MCP server (integration test)
    ListTools {
        /// MCP server name from config
        server: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let driver_config = config::load_config(&cli.config)?;

    match cli.command {
        Commands::Serve { port, bind } => {
            let p = port.unwrap_or(driver_config.server.http_port);
            serve::start(driver_config, &bind, p).await
        }
        Commands::ListTools { server } => cmd_list_tools(&driver_config, &server).await,
    }
}

async fn cmd_list_tools(driver_config: &config::DriverConfig, server_name: &str) -> Result<()> {
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

    let init_result = client.initialize().await?;
    println!("Server: {:?}", init_result.get("serverInfo"));

    let tools = client.list_tools().await?;
    println!("\n{} tools:", tools.len());
    for tool in &tools {
        let desc = tool.description.as_deref().unwrap_or("(no description)");
        let desc_short = if desc.len() > 80 {
            format!("{}...", &desc[..80])
        } else {
            desc.to_string()
        };
        println!("  {} - {}", tool.name, desc_short);
    }

    client.shutdown().await?;
    Ok(())
}
