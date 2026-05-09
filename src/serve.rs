use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use crate::config::{DriverConfig, RunRequest};
use crate::driver_tools::DriverTools;
use crate::events::{ChannelSink, EventSink, EventWriter};
use crate::report;
use crate::run;

pub struct AppState {
    pub config: DriverConfig,
    pub start_time: std::time::Instant,
}

pub fn build_router(config: DriverConfig) -> Router {
    let state = Arc::new(AppState {
        config,
        start_time: std::time::Instant::now(),
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/health", get(health))
        .route("/registry", get(registry))
        .route("/run", post(run_sse))
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
        .layer(cors)
        .with_state(state)
}

// ── GET /health ──

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let uptime = state.start_time.elapsed().as_secs();
    Json(json!({
        "status": "ok",
        "uptime_secs": uptime,
        "registered_models": state.config.models.len(),
        "registered_servers": state.config.mcp_servers.len(),
    }))
}

// ── GET /registry ──

async fn registry(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let models: Vec<serde_json::Value> = state
        .config
        .models
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "model_id": m.model_id,
                "base_url": m.base_url,
                "max_tokens": m.max_tokens,
                "temperature": m.temperature,
            })
        })
        .collect();

    let mcp_servers: Vec<serde_json::Value> = state
        .config
        .mcp_servers
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "command": s.command,
                "tools": "listed at spawn time"
            })
        })
        .collect();

    let driver_tools: Vec<String> = DriverTools::tool_definitions()
        .iter()
        .map(|t| t.name.clone())
        .collect();

    Json(json!({
        "models": models,
        "mcp_servers": mcp_servers,
        "driver_tools": driver_tools,
    }))
}

// ── POST /run → SSE stream ──

async fn run_sse(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunRequest>,
) -> Result<
    Sse<impl Stream<Item = Result<SseEvent, Infallible>>>,
    (StatusCode, Json<serde_json::Value>),
> {
    // Validate model exists
    if state.config.find_model(&req.model).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("model '{}' not found", req.model)})),
        ));
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let task_name = req.name.as_deref().unwrap_or("unnamed");
    info!("POST /run: run_id={} model={} task={}", run_id, req.model, task_name);

    let (tx, rx) = mpsc::channel(256);
    let config = state.config.clone();
    let run_id_clone = run_id.clone();

    tokio::spawn(async move {
        // Create run directory and file event writer
        let run_dir = std::path::PathBuf::from(&config.server.log_dir)
            .join("runs")
            .join(&run_id_clone);

        let mut channel_sink = ChannelSink::new(tx);

        // Try to create file writer for dual logging
        if let Ok(file_writer) = EventWriter::new(&run_dir) {
            let mut dual = crate::events::DualSink::new(file_writer, &mut channel_sink);
            let result = run::run_task(&config, &req, &run_id_clone, &mut dual).await;
            if let Err(e) = result {
                let _ = dual.log("error", json!({"error": e.to_string()}));
            }
            // Write report from JSONL
            let _ = report::write_report(&run_dir);
        } else {
            let result = run::run_task(&config, &req, &run_id_clone, &mut channel_sink).await;
            if let Err(e) = result {
                let _ = channel_sink.log("error", json!({"error": e.to_string()}));
            }
        }
    });

    let stream = ReceiverStream::new(rx);
    let sse_stream = futures_util::stream::StreamExt::map(stream, |event| {
        let data = serde_json::to_string(&event).unwrap_or_default();
        Ok(SseEvent::default().event(&event.kind).data(data))
    });

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}

// ── GET /runs?limit=N&offset=N ──

#[derive(Deserialize)]
struct RunsQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_limit() -> usize {
    20
}

async fn list_runs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<RunsQuery>,
) -> Json<serde_json::Value> {
    let runs_dir = std::path::PathBuf::from(&state.config.server.log_dir).join("runs");

    let mut runs = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&runs_dir) {
        let mut dirs: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();

        // Sort by name descending (most recent first since UUIDs are random, but we'll use mtime)
        dirs.sort_by(|a, b| {
            let a_time = a.metadata().and_then(|m| m.modified()).ok();
            let b_time = b.metadata().and_then(|m| m.modified()).ok();
            b_time.cmp(&a_time)
        });

        for entry in dirs.into_iter().skip(query.offset).take(query.limit) {
            let id = entry.file_name().to_string_lossy().to_string();
            let jsonl_path = entry.path().join("run.jsonl");

            let mut run_info = json!({"id": id});

            if let Ok(content) = std::fs::read_to_string(&jsonl_path) {
                // Read first and last lines for metadata
                let lines: Vec<&str> = content.lines().collect();
                if let Some(first) = lines.first() {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(first) {
                        if let Some(name) = v.get("name").or(v.get("task")) {
                            run_info["name"] = name.clone();
                        }
                        if let Some(model) = v.get("model") {
                            run_info["model"] = model.clone();
                        }
                        if let Some(ts) = v.get("ts") {
                            run_info["started_at"] = ts.clone();
                        }
                    }
                }
                if let Some(last) = lines.last() {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(last) {
                        if let Some(ok) = v.get("ok") {
                            run_info["ok"] = ok.clone();
                        }
                        if let Some(dur) = v.get("duration_ms") {
                            run_info["duration_ms"] = dur.clone();
                        }
                    }
                }
            }

            runs.push(run_info);
        }
    }

    Json(json!({"runs": runs, "total": runs.len()}))
}

// ── GET /runs/:id ──

async fn get_run(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let run_dir = std::path::PathBuf::from(&state.config.server.log_dir)
        .join("runs")
        .join(&id);

    if !run_dir.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("run '{}' not found", id)})),
        ));
    }

    let jsonl_path = run_dir.join("run.jsonl");
    let events: Vec<serde_json::Value> = if let Ok(content) = std::fs::read_to_string(&jsonl_path)
    {
        content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    } else {
        Vec::new()
    };

    let report = run_dir.join("shared_state.md");
    let report_content = std::fs::read_to_string(&report).unwrap_or_default();

    Ok(Json(json!({
        "id": id,
        "events": events,
        "report": report_content,
    })))
}

pub async fn start(config: DriverConfig, bind: &str, port: u16) -> anyhow::Result<()> {
    // Ensure state directories exist
    let log_dir = &config.server.log_dir;
    let _ = std::fs::create_dir_all(format!("{}/runs", log_dir));
    let _ = std::fs::create_dir_all(format!("{}/breadcrumbs/active", log_dir));
    let _ = std::fs::create_dir_all(format!("{}/breadcrumbs/completed", log_dir));
    let _ = std::fs::create_dir_all(format!("{}/loaves", log_dir));

    let router = build_router(config);
    let addr = format!("{}:{}", bind, port);
    info!("driver listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}
