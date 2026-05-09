use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::mcp::{error_result, text_result, ToolCallResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub step_idx: usize,
    pub step_name: String,
    pub result: String,
    pub at: String,
    #[serde(default)]
    pub files_changed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breadcrumb {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub owner: String,
    pub started_at: String,
    pub last_activity_at: String,
    pub steps: Vec<String>,
    pub current_step: usize,
    pub total_steps: usize,
    pub step_results: Vec<StepResult>,
    pub files_changed: Vec<String>,
    #[serde(default)]
    pub aborted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abort_reason: Option<String>,
}

pub struct Breadcrumbs {
    active_dir: PathBuf,
    completed_base: PathBuf,
}

impl Breadcrumbs {
    pub fn new(log_dir: &str) -> Self {
        let base = PathBuf::from(log_dir).join("breadcrumbs");
        let active_dir = base.join("active");
        let completed_base = base.join("completed");
        let _ = fs::create_dir_all(&active_dir);
        let _ = fs::create_dir_all(&completed_base);
        Breadcrumbs {
            active_dir,
            completed_base,
        }
    }

    fn active_path(&self, id: &str) -> PathBuf {
        self.active_dir.join(format!("{}.json", id))
    }

    fn load_active(&self, id: &str) -> Option<Breadcrumb> {
        let path = self.active_path(id);
        let content = fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save_active(&self, bc: &Breadcrumb) -> Result<(), String> {
        let path = self.active_path(&bc.id);
        let content = serde_json::to_string_pretty(bc).map_err(|e| e.to_string())?;
        fs::write(path, content).map_err(|e| e.to_string())
    }

    fn load_all_active(&self) -> Vec<Breadcrumb> {
        let mut result = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.active_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().map(|e| e == "json").unwrap_or(false) {
                    if let Ok(content) = fs::read_to_string(entry.path()) {
                        if let Ok(bc) = serde_json::from_str::<Breadcrumb>(&content) {
                            result.push(bc);
                        }
                    }
                }
            }
        }
        result
    }

    fn resolve_id(&self, explicit: Option<&str>) -> Result<String, String> {
        if let Some(id) = explicit {
            return Ok(id.to_string());
        }
        let active = self.load_all_active();
        match active.len() {
            0 => Err("no active breadcrumb".to_string()),
            1 => Ok(active[0].id.clone()),
            n => Err(format!(
                "ambiguous: {} active breadcrumbs, provide breadcrumb_id",
                n
            )),
        }
    }

    fn slugify(name: &str, max_len: usize) -> String {
        name.to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .take(max_len)
            .collect::<String>()
            .trim_end_matches('_')
            .to_string()
    }

    fn new_id(name: &str) -> String {
        let ts = Utc::now().timestamp();
        let slug = Self::slugify(name, 40);
        format!("bc_{}_{}", ts, slug)
    }

    pub fn start(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let name = args["name"].as_str().unwrap_or("unnamed");
        let steps: Vec<String> = args["steps"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let project_id = args["project_id"].as_str().map(String::from);

        if steps.is_empty() {
            return Ok(error_result("steps[] is required and must not be empty"));
        }

        let id = Self::new_id(name);
        let now = Utc::now().to_rfc3339();
        let total = steps.len();

        let bc = Breadcrumb {
            id: id.clone(),
            name: name.to_string(),
            project_id: project_id.clone(),
            owner: "driver".to_string(),
            started_at: now.clone(),
            last_activity_at: now,
            steps,
            current_step: 0,
            total_steps: total,
            step_results: Vec::new(),
            files_changed: Vec::new(),
            aborted: false,
            abort_reason: None,
        };

        if let Err(e) = self.save_active(&bc) {
            return Ok(error_result(format!("failed to save breadcrumb: {}", e)));
        }

        Ok(text_result(serde_json::to_string_pretty(&json!({
            "id": id,
            "name": name,
            "project_id": project_id,
            "status": "started",
            "total_steps": total
        }))?))
    }

    pub fn step(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let result_text = args["result"].as_str().unwrap_or("");
        let files_changed: Vec<String> = args["files_changed"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let explicit_id = args["breadcrumb_id"].as_str();

        let id = match self.resolve_id(explicit_id) {
            Ok(id) => id,
            Err(e) => return Ok(error_result(e)),
        };

        let mut bc = match self.load_active(&id) {
            Some(bc) => bc,
            None => return Ok(error_result(format!("breadcrumb '{}' not found", id))),
        };

        let step_idx = bc.current_step;
        let step_name = bc.steps.get(step_idx).cloned().unwrap_or_else(|| format!("step_{}", step_idx));

        bc.step_results.push(StepResult {
            step_idx,
            step_name: step_name.clone(),
            result: result_text.to_string(),
            at: Utc::now().to_rfc3339(),
            files_changed: files_changed.clone(),
        });

        bc.files_changed.extend(files_changed);
        bc.current_step += 1;
        bc.last_activity_at = Utc::now().to_rfc3339();

        if let Err(e) = self.save_active(&bc) {
            return Ok(error_result(format!("failed to save: {}", e)));
        }

        Ok(text_result(serde_json::to_string_pretty(&json!({
            "id": id,
            "step_completed": step_name,
            "progress": format!("{}/{}", bc.current_step, bc.total_steps)
        }))?))
    }

    pub fn complete(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let summary = args["summary"].as_str().unwrap_or("completed");
        let explicit_id = args["breadcrumb_id"].as_str();

        let id = match self.resolve_id(explicit_id) {
            Ok(id) => id,
            Err(e) => return Ok(error_result(e)),
        };

        let bc = match self.load_active(&id) {
            Some(bc) => bc,
            None => return Ok(error_result(format!("breadcrumb '{}' not found", id))),
        };

        // Archive to completed/<date>/
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let completed_dir = self.completed_base.join(&date);
        let _ = fs::create_dir_all(&completed_dir);
        let dest = completed_dir.join(format!("{}.json", id));
        let content = serde_json::to_string_pretty(&bc)?;
        fs::write(&dest, content).map_err(|e| anyhow::anyhow!("archive write: {}", e))?;

        // Remove from active
        let _ = fs::remove_file(self.active_path(&id));

        Ok(text_result(serde_json::to_string_pretty(&json!({
            "id": id,
            "status": "completed",
            "summary": summary,
            "archived_to": dest.display().to_string(),
            "steps_completed": bc.current_step,
            "total_steps": bc.total_steps,
            "files_changed": bc.files_changed,
        }))?))
    }

    pub fn abort(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let reason = args["reason"].as_str().unwrap_or("aborted");
        let explicit_id = args["breadcrumb_id"].as_str();

        let id = match self.resolve_id(explicit_id) {
            Ok(id) => id,
            Err(e) => return Ok(error_result(e)),
        };

        let mut bc = match self.load_active(&id) {
            Some(bc) => bc,
            None => return Ok(error_result(format!("breadcrumb '{}' not found", id))),
        };

        bc.aborted = true;
        bc.abort_reason = Some(reason.to_string());
        bc.last_activity_at = Utc::now().to_rfc3339();

        // Archive as aborted
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let completed_dir = self.completed_base.join(&date);
        let _ = fs::create_dir_all(&completed_dir);
        let dest = completed_dir.join(format!("{}.json", id));
        fs::write(&dest, serde_json::to_string_pretty(&bc)?)?;
        let _ = fs::remove_file(self.active_path(&id));

        Ok(text_result(serde_json::to_string_pretty(&json!({
            "id": id,
            "status": "aborted",
            "reason": reason
        }))?))
    }

    pub fn status(&self, _args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let active = self.load_all_active();
        if active.is_empty() {
            return Ok(text_result("no active breadcrumbs"));
        }
        let summaries: Vec<Value> = active
            .iter()
            .map(|bc| {
                json!({
                    "id": bc.id,
                    "name": bc.name,
                    "progress": format!("{}/{}", bc.current_step, bc.total_steps),
                    "started_at": bc.started_at,
                    "last_activity": bc.last_activity_at,
                    "project_id": bc.project_id,
                })
            })
            .collect();
        Ok(text_result(serde_json::to_string_pretty(&summaries)?))
    }

    pub fn list(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let scope = args["scope"].as_str().unwrap_or("active");

        match scope {
            "active" => self.status(args),
            "today" | "week" | "all" => {
                let mut all = Vec::new();
                // Active
                all.extend(self.load_all_active().into_iter().map(|bc| {
                    json!({"id": bc.id, "name": bc.name, "status": "active", "started_at": bc.started_at})
                }));
                // Completed dirs
                if let Ok(entries) = fs::read_dir(&self.completed_base) {
                    let today = Utc::now().format("%Y-%m-%d").to_string();
                    for entry in entries.flatten() {
                        let dir_name = entry.file_name().to_string_lossy().to_string();
                        let include = match scope {
                            "today" => dir_name == today,
                            "week" => true, // simplified: include all for now
                            "all" => true,
                            _ => false,
                        };
                        if include {
                            if let Ok(files) = fs::read_dir(entry.path()) {
                                for f in files.flatten() {
                                    if let Ok(content) = fs::read_to_string(f.path()) {
                                        if let Ok(bc) = serde_json::from_str::<Breadcrumb>(&content) {
                                            let status = if bc.aborted { "aborted" } else { "completed" };
                                            all.push(json!({
                                                "id": bc.id,
                                                "name": bc.name,
                                                "status": status,
                                                "started_at": bc.started_at,
                                            }));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(text_result(serde_json::to_string_pretty(&all)?))
            }
            _ => Ok(error_result(format!("unknown scope: {}", scope))),
        }
    }

    pub fn adopt(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let id = args["breadcrumb_id"].as_str().unwrap_or("");
        if id.is_empty() {
            return Ok(error_result("breadcrumb_id is required"));
        }
        let mut bc = match self.load_active(id) {
            Some(bc) => bc,
            None => return Ok(error_result(format!("breadcrumb '{}' not found", id))),
        };
        bc.owner = "driver".to_string();
        bc.last_activity_at = Utc::now().to_rfc3339();
        if let Err(e) = self.save_active(&bc) {
            return Ok(error_result(format!("failed to save: {}", e)));
        }
        Ok(text_result(format!("breadcrumb '{}' adopted by driver", id)))
    }
}
