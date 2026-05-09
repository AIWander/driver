use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use crate::mcp::{error_result, text_result, ToolCallResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Loaf {
    loaf_id: String,
    goal: String,
    created: String,
    status: String,
    current_phase: usize,
    #[serde(default)]
    phases: Vec<Phase>,
    #[serde(default)]
    decisions: Vec<Decision>,
    #[serde(default)]
    discoveries: Vec<Discovery>,
    #[serde(default)]
    next_actions: Vec<String>,
    #[serde(default)]
    breadcrumbs: Vec<LogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Phase {
    name: String,
    status: String,
    #[serde(default)]
    tasks: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Decision {
    what: String,
    #[serde(default)]
    why: Option<String>,
    #[serde(default)]
    who: Option<String>,
    when: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Discovery {
    what: String,
    #[serde(default)]
    impact: Option<String>,
    when: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogEntry {
    timestamp: String,
    event: String,
}

pub struct Loafs {
    loaves_dir: PathBuf,
    archive_dir: PathBuf,
}

impl Loafs {
    pub fn new(log_dir: &str) -> Self {
        let base = PathBuf::from(log_dir).join("loaves");
        let archive = base.join("archive");
        let _ = fs::create_dir_all(&base);
        let _ = fs::create_dir_all(&archive);
        Loafs {
            loaves_dir: base,
            archive_dir: archive,
        }
    }

    fn loaf_path(&self, id: &str) -> PathBuf {
        self.loaves_dir.join(format!("{}.json", id))
    }

    fn load(&self, id: &str) -> Option<Loaf> {
        let content = fs::read_to_string(self.loaf_path(id)).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save(&self, loaf: &Loaf) -> Result<(), String> {
        let content = serde_json::to_string_pretty(loaf).map_err(|e| e.to_string())?;
        fs::write(self.loaf_path(&loaf.loaf_id), content).map_err(|e| e.to_string())
    }

    fn find_active(&self) -> Option<Loaf> {
        if let Ok(entries) = fs::read_dir(&self.loaves_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().map(|e| e == "json").unwrap_or(false) {
                    if let Ok(content) = fs::read_to_string(entry.path()) {
                        if let Ok(loaf) = serde_json::from_str::<Loaf>(&content) {
                            if loaf.status == "active" {
                                return Some(loaf);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    pub fn create(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let project_name = args["project_name"].as_str().unwrap_or("unnamed");
        let goal = args["goal"].as_str().unwrap_or("");
        let phase_names: Vec<String> = args["phases"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        if goal.is_empty() {
            return Ok(error_result("goal is required"));
        }

        let loaf_id = format!(
            "{}_Loaf",
            project_name
                .replace(' ', "_")
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        );

        let phases: Vec<Phase> = if phase_names.is_empty() {
            vec![Phase {
                name: "default".to_string(),
                status: "active".to_string(),
                tasks: Vec::new(),
            }]
        } else {
            phase_names
                .iter()
                .enumerate()
                .map(|(i, name)| Phase {
                    name: name.clone(),
                    status: if i == 0 { "active" } else { "pending" }.to_string(),
                    tasks: Vec::new(),
                })
                .collect()
        };

        let now = Utc::now().to_rfc3339();
        let loaf = Loaf {
            loaf_id: loaf_id.clone(),
            goal: goal.to_string(),
            created: now.clone(),
            status: "active".to_string(),
            current_phase: 0,
            phases,
            decisions: Vec::new(),
            discoveries: Vec::new(),
            next_actions: Vec::new(),
            breadcrumbs: vec![LogEntry {
                timestamp: now,
                event: "Loaf created".to_string(),
            }],
        };

        if let Err(e) = self.save(&loaf) {
            return Ok(error_result(format!("failed to save loaf: {}", e)));
        }

        Ok(text_result(serde_json::to_string_pretty(&json!({
            "loaf_id": loaf_id,
            "goal": goal,
            "status": "active",
            "phases": loaf.phases.len()
        }))?))
    }

    pub fn update(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let loaf_id = args["loaf_id"].as_str().unwrap_or("");
        if loaf_id.is_empty() {
            return Ok(error_result("loaf_id is required"));
        }

        let mut loaf = match self.load(loaf_id) {
            Some(l) => l,
            None => return Ok(error_result(format!("loaf '{}' not found", loaf_id))),
        };

        let now = Utc::now().to_rfc3339();

        // Task update
        if let Some(task) = args.get("task_update") {
            let task_id = task["task_id"].as_str().unwrap_or("unknown");
            let status = task["status"].as_str().unwrap_or("done");
            if let Some(phase) = loaf.phases.get_mut(loaf.current_phase) {
                phase.tasks.push(task.clone());
            }
            loaf.breadcrumbs.push(LogEntry {
                timestamp: now.clone(),
                event: format!("Task {} -> {}", task_id, status),
            });
        }

        // Decision
        if let Some(d) = args.get("decision") {
            let what = d["what"].as_str().unwrap_or("");
            loaf.decisions.push(Decision {
                what: what.to_string(),
                why: d["why"].as_str().map(String::from),
                who: d["who"].as_str().map(String::from),
                when: now.clone(),
            });
            loaf.breadcrumbs.push(LogEntry {
                timestamp: now.clone(),
                event: format!("Decision: {}", what),
            });
        }

        // Discovery
        if let Some(d) = args.get("discovery") {
            let what = d["what"].as_str().unwrap_or("");
            loaf.discoveries.push(Discovery {
                what: what.to_string(),
                impact: d["impact"].as_str().map(String::from),
                when: now.clone(),
            });
            loaf.breadcrumbs.push(LogEntry {
                timestamp: now.clone(),
                event: format!("Discovery: {}", what),
            });
        }

        // Next actions
        if let Some(actions) = args["next_actions"].as_array() {
            loaf.next_actions = actions
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            loaf.breadcrumbs.push(LogEntry {
                timestamp: now.clone(),
                event: "Next actions updated".to_string(),
            });
        }

        // Phase advancement
        if let Some(status) = args["phase_status"].as_str() {
            if status == "done" {
                if let Some(phase) = loaf.phases.get_mut(loaf.current_phase) {
                    phase.status = "done".to_string();
                }
                loaf.current_phase += 1;
                if let Some(next_phase) = loaf.phases.get_mut(loaf.current_phase) {
                    next_phase.status = "active".to_string();
                }
                loaf.breadcrumbs.push(LogEntry {
                    timestamp: now.clone(),
                    event: format!("Phase advanced to: {}", loaf.current_phase),
                });
            }
        }

        if let Err(e) = self.save(&loaf) {
            return Ok(error_result(format!("save failed: {}", e)));
        }

        Ok(text_result(format!("loaf '{}' updated", loaf_id)))
    }

    pub fn status(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let loaf_id = args["loaf_id"].as_str();

        let loaf = if let Some(id) = loaf_id {
            match self.load(id) {
                Some(l) => l,
                None => return Ok(error_result(format!("loaf '{}' not found", id))),
            }
        } else {
            match self.find_active() {
                Some(l) => l,
                None => return Ok(text_result("no active loaf")),
            }
        };

        let total_tasks: usize = loaf.phases.iter().map(|p| p.tasks.len()).sum();
        let last_breadcrumbs: Vec<&LogEntry> = loaf.breadcrumbs.iter().rev().take(5).collect();

        Ok(text_result(serde_json::to_string_pretty(&json!({
            "loaf_id": loaf.loaf_id,
            "goal": loaf.goal,
            "status": loaf.status,
            "current_phase": loaf.current_phase,
            "total_phases": loaf.phases.len(),
            "total_tasks": total_tasks,
            "decisions_count": loaf.decisions.len(),
            "discoveries_count": loaf.discoveries.len(),
            "next_actions": loaf.next_actions,
            "last_events": last_breadcrumbs,
        }))?))
    }

    pub fn close(&self, args: Value) -> Result<ToolCallResult, anyhow::Error> {
        let loaf_id = args["loaf_id"].as_str().unwrap_or("");
        if loaf_id.is_empty() {
            return Ok(error_result("loaf_id is required"));
        }

        let mut loaf = match self.load(loaf_id) {
            Some(l) => l,
            None => return Ok(error_result(format!("loaf '{}' not found", loaf_id))),
        };

        loaf.status = "completed".to_string();
        loaf.breadcrumbs.push(LogEntry {
            timestamp: Utc::now().to_rfc3339(),
            event: "Loaf closed".to_string(),
        });

        // Archive
        let dest = self.archive_dir.join(format!("{}.json", loaf_id));
        let content = serde_json::to_string_pretty(&loaf)?;
        fs::write(&dest, content)?;
        let _ = fs::remove_file(self.loaf_path(loaf_id));

        Ok(text_result(serde_json::to_string_pretty(&json!({
            "loaf_id": loaf_id,
            "status": "completed",
            "archived_to": dest.display().to_string()
        }))?))
    }
}
