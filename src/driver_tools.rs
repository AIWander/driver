use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::breadcrumbs::Breadcrumbs;
use crate::loafs::Loafs;
use crate::mcp::{error_result, text_result, McpTool, ToolCallResult};
use crate::policy;

// ── Helpers ──

fn tool(name: &str, desc: &str, schema: Value) -> McpTool {
    McpTool {
        name: name.to_string(),
        description: Some(desc.to_string()),
        input_schema: schema,
    }
}

// ── Shell Session ──

struct ShellSession {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    history: Vec<(String, String)>,
    cwd: String,
    created_at: String,
}

// ── DriverTools ──

pub struct DriverTools {
    sessions: HashMap<String, ShellSession>,
    pub breadcrumbs: Breadcrumbs,
    pub loafs: Loafs,
}

impl DriverTools {
    pub fn new(log_dir: &str) -> Self {
        DriverTools {
            sessions: HashMap::new(),
            breadcrumbs: Breadcrumbs::new(log_dir),
            loafs: Loafs::new(log_dir),
        }
    }

    pub fn tool_definitions() -> Vec<McpTool> {
        let mut tools = Vec::new();
        tools.extend(filesystem_tools());
        tools.extend(shell_tools());
        tools.extend(session_tools());
        tools.extend(transform_tools());
        tools.extend(git_tools());
        tools.extend(http_tools());
        tools.extend(system_tools());
        tools.extend(breadcrumb_tools());
        tools.extend(loaf_tools());
        tools
    }

    pub async fn dispatch(&mut self, name: &str, args: Value) -> Result<ToolCallResult> {
        match name {
            // Filesystem
            "read_file" => self.read_file(args).await,
            "write_file" => self.write_file(args).await,
            "append_file" => self.append_file(args).await,
            "list_dir" => self.list_dir(args).await,
            "archive_create" => self.archive_create(args).await,
            "archive_extract" => self.archive_extract(args).await,
            // Shell
            "bash" => self.bash(args).await,
            "run" => self.run_cmd(args).await,
            // Sessions
            "session_create" => self.session_create(args).await,
            "session_run" => self.session_run(args).await,
            "session_destroy" => self.session_destroy(args).await,
            "session_cd" => self.session_cd(args).await,
            "session_set_env" => self.session_set_env(args).await,
            "session_get_env" => self.session_get_env(args).await,
            "session_list" => self.session_list(args).await,
            "session_history" => self.session_history(args).await,
            // Transforms
            "transform_grep" => self.transform_grep(args).await,
            "transform_find_replace" => self.transform_find_replace(args).await,
            "transform_diff_files" => self.transform_diff_files(args).await,
            "transform_extract_lines" => self.transform_extract_lines(args).await,
            "transform_json_format" => self.transform_json_format(args).await,
            "transform_hash_file" => self.transform_hash_file(args).await,
            "transform_file_stats" => self.transform_file_stats(args).await,
            // Git
            "git_status" => self.git_status(args).await,
            "git_diff" => self.git_diff(args).await,
            "git_commit" => self.git_commit(args).await,
            "git_log" => self.git_log(args).await,
            "git_stash" => self.git_stash(args).await,
            "git_branch" => self.git_branch(args).await,
            "git_checkout" => self.git_checkout(args).await,
            "git_reset" => self.git_reset(args).await,
            // HTTP
            "http_request" => self.http_request(args).await,
            "http_fetch" => self.http_fetch(args).await,
            "port_check" => self.port_check(args).await,
            // System
            "system_info" => self.system_info(args).await,
            "kill_process" => self.kill_process(args).await,
            "list_process" => self.list_process(args).await,
            // Breadcrumbs
            "breadcrumb_start" => self.breadcrumbs.start(args),
            "breadcrumb_step" => self.breadcrumbs.step(args),
            "breadcrumb_complete" => self.breadcrumbs.complete(args),
            "breadcrumb_abort" => self.breadcrumbs.abort(args),
            "breadcrumb_status" => self.breadcrumbs.status(args),
            "breadcrumb_list" => self.breadcrumbs.list(args),
            "breadcrumb_adopt" => self.breadcrumbs.adopt(args),
            // Loafs
            "loaf_create" => self.loafs.create(args),
            "loaf_update" => self.loafs.update(args),
            "loaf_status" => self.loafs.status(args),
            "loaf_close" => self.loafs.close(args),

            _ => Ok(error_result(format!("unknown driver tool: {}", name))),
        }
    }

    // ═══════════════════════════════════════════
    // FILESYSTEM
    // ═══════════════════════════════════════════

    async fn read_file(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(error_result("path is required"));
        }

        let max_kb = args["max_kb"].as_u64().unwrap_or(100);
        let max_bytes = max_kb * 1024;

        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return Ok(error_result(format!("cannot stat {}: {}", path, e))),
        };

        if metadata.len() > max_bytes {
            return Ok(error_result(format!(
                "file is {} KB, exceeds max_kb={} KB. Use lines or search to read a portion.",
                metadata.len() / 1024,
                max_kb
            )));
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return Ok(error_result(format!("cannot read {}: {}", path, e))),
        };

        // Optional line range
        if let Some(lines_val) = args.get("lines") {
            if let Some(lines_str) = lines_val.as_str() {
                let parts: Vec<&str> = lines_str.split('-').collect();
                if parts.len() == 2 {
                    let start: usize = parts[0].parse().unwrap_or(1);
                    let end: usize = parts[1].parse().unwrap_or(usize::MAX);
                    let selected: String = content
                        .lines()
                        .enumerate()
                        .filter(|(i, _)| *i + 1 >= start && *i + 1 <= end)
                        .map(|(i, l)| format!("{:>5}\t{}", i + 1, l))
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Ok(text_result(selected));
                }
            }
        }

        // Optional in-file search
        if let Some(search) = args["search"].as_str() {
            let matching: String = content
                .lines()
                .enumerate()
                .filter(|(_, l)| l.contains(search))
                .map(|(i, l)| format!("{:>5}\t{}", i + 1, l))
                .collect::<Vec<_>>()
                .join("\n");
            if matching.is_empty() {
                return Ok(text_result(format!("no lines matching '{}' in {}", search, path)));
            }
            return Ok(text_result(matching));
        }

        // Number lines
        let numbered: String = content
            .lines()
            .enumerate()
            .map(|(i, l)| format!("{:>5}\t{}", i + 1, l))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(text_result(numbered))
    }

    async fn write_file(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or("");
        let content = args["content"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(error_result("path is required"));
        }
        if let Some(parent) = Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(path, content) {
            Ok(_) => Ok(text_result(format!("wrote {} bytes to {}", content.len(), path))),
            Err(e) => Ok(error_result(format!("write failed: {}", e))),
        }
    }

    async fn append_file(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or("");
        let content = args["content"].as_str().unwrap_or("");
        if path.is_empty() {
            return Ok(error_result("path is required"));
        }
        use std::io::Write;
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut f) => {
                match f.write_all(content.as_bytes()) {
                    Ok(_) => Ok(text_result(format!("appended {} bytes to {}", content.len(), path))),
                    Err(e) => Ok(error_result(format!("append failed: {}", e))),
                }
            }
            Err(e) => Ok(error_result(format!("open failed: {}", e))),
        }
    }

    async fn list_dir(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or(".");
        let depth = args["depth"].as_u64().unwrap_or(2) as usize;

        let mut lines = Vec::new();
        for entry in walkdir::WalkDir::new(path)
            .max_depth(depth)
            .sort_by_file_name()
        {
            match entry {
                Ok(e) => {
                    let indent = "  ".repeat(e.depth());
                    let name = e.file_name().to_string_lossy();
                    let suffix = if e.file_type().is_dir() { "/" } else { "" };
                    lines.push(format!("{}{}{}", indent, name, suffix));
                }
                Err(e) => lines.push(format!("  error: {}", e)),
            }
        }
        Ok(text_result(lines.join("\n")))
    }

    async fn archive_create(&self, args: Value) -> Result<ToolCallResult> {
        let output = args["output"].as_str().unwrap_or("");
        let paths = args["paths"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if output.is_empty() || paths.is_empty() {
            return Ok(error_result("output and paths[] are required"));
        }
        let paths_str = paths
            .iter()
            .map(|p| format!("'{}'", p))
            .collect::<Vec<_>>()
            .join(" ");
        let cmd = format!("tar czf '{}' {}", output, paths_str);
        run_bash(&cmd, None, 60).await
    }

    async fn archive_extract(&self, args: Value) -> Result<ToolCallResult> {
        let archive = args["archive"].as_str().unwrap_or("");
        let dest = args["destination"].as_str().unwrap_or(".");
        if archive.is_empty() {
            return Ok(error_result("archive is required"));
        }
        let cmd = format!("tar xzf '{}' -C '{}'", archive, dest);
        run_bash(&cmd, None, 60).await
    }

    // ═══════════════════════════════════════════
    // SHELL
    // ═══════════════════════════════════════════

    async fn bash(&self, args: Value) -> Result<ToolCallResult> {
        let command = args["command"].as_str().unwrap_or("");
        if command.is_empty() {
            return Ok(error_result("command is required"));
        }
        let confirm = args["confirm"].as_bool().unwrap_or(false);
        let allow_destructive = args["allow_destructive"].as_bool().unwrap_or(false);
        let timeout = args["timeout_secs"].as_u64().unwrap_or(120);
        let working_dir = args["working_dir"].as_str();

        if let Err(msg) = policy::check_command(command, confirm, allow_destructive) {
            return Ok(error_result(msg));
        }

        run_bash(command, working_dir, timeout).await
    }

    async fn run_cmd(&self, args: Value) -> Result<ToolCallResult> {
        let command = args["command"].as_str().unwrap_or("");
        if command.is_empty() {
            return Ok(error_result("command is required"));
        }
        run_bash(command, None, 30).await
    }

    // ═══════════════════════════════════════════
    // PERSISTENT SESSIONS
    // ═══════════════════════════════════════════

    async fn session_create(&mut self, args: Value) -> Result<ToolCallResult> {
        let name = args["name"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| format!("s{}", self.sessions.len()));
        let cwd = args["cwd"].as_str().unwrap_or("/root");

        if self.sessions.contains_key(&name) {
            return Ok(error_result(format!("session '{}' already exists", name)));
        }

        let mut child = tokio::process::Command::new("bash")
            .arg("--norc")
            .arg("--noprofile")
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        self.sessions.insert(
            name.clone(),
            ShellSession {
                child,
                stdin,
                reader: BufReader::new(stdout),
                history: Vec::new(),
                cwd: cwd.to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
            },
        );

        Ok(text_result(format!("session '{}' created in {}", name, cwd)))
    }

    async fn session_run(&mut self, args: Value) -> Result<ToolCallResult> {
        let session_name = args["session"].as_str().unwrap_or("");
        let command = args["command"].as_str().unwrap_or("");
        if session_name.is_empty() || command.is_empty() {
            return Ok(error_result("session and command are required"));
        }

        let confirm = args["confirm"].as_bool().unwrap_or(false);
        let allow_destructive = args["allow_destructive"].as_bool().unwrap_or(false);
        if let Err(msg) = policy::check_command(command, confirm, allow_destructive) {
            return Ok(error_result(msg));
        }

        let session = match self.sessions.get_mut(session_name) {
            Some(s) => s,
            None => return Ok(error_result(format!("session '{}' not found", session_name))),
        };

        let sentinel = format!("__DRIVER_SENTINEL_{}__", uuid::Uuid::new_v4());
        let full_cmd = format!(
            "{} 2>&1; echo \"{}\"\n",
            command, sentinel
        );

        session.stdin.write_all(full_cmd.as_bytes()).await?;
        session.stdin.flush().await?;

        let mut output = String::new();
        let timeout = tokio::time::Duration::from_secs(120);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if tokio::time::Instant::now() > deadline {
                output.push_str("\n[timeout after 120s]");
                break;
            }
            let mut line = String::new();
            match tokio::time::timeout(timeout, session.reader.read_line(&mut line)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(_)) => {
                    if line.trim() == sentinel {
                        break;
                    }
                    output.push_str(&line);
                }
                Ok(Err(e)) => {
                    output.push_str(&format!("\n[read error: {}]", e));
                    break;
                }
                Err(_) => {
                    output.push_str("\n[timeout]");
                    break;
                }
            }
        }

        session
            .history
            .push((command.to_string(), output.clone()));
        Ok(text_result(output))
    }

    async fn session_destroy(&mut self, args: Value) -> Result<ToolCallResult> {
        let name = args["session"].as_str().unwrap_or("");
        if let Some(mut session) = self.sessions.remove(name) {
            drop(session.stdin);
            let _ = session.child.kill().await;
            Ok(text_result(format!("session '{}' destroyed", name)))
        } else {
            Ok(error_result(format!("session '{}' not found", name)))
        }
    }

    async fn session_cd(&mut self, args: Value) -> Result<ToolCallResult> {
        let name = args["session"].as_str().unwrap_or("");
        let path = args["path"].as_str().unwrap_or("");
        if name.is_empty() || path.is_empty() {
            return Ok(error_result("session and path are required"));
        }
        let cd_args = json!({"session": name, "command": format!("cd '{}' && pwd", path)});
        let result = self.session_run(cd_args).await?;
        if !result.is_error {
            if let Some(s) = self.sessions.get_mut(name) {
                s.cwd = result.text_content().trim().to_string();
            }
        }
        Ok(result)
    }

    async fn session_set_env(&mut self, args: Value) -> Result<ToolCallResult> {
        let name = args["session"].as_str().unwrap_or("");
        let key = args["key"].as_str().unwrap_or("");
        let value = args["value"].as_str().unwrap_or("");
        let cmd_args = json!({"session": name, "command": format!("export {}='{}'", key, value)});
        self.session_run(cmd_args).await
    }

    async fn session_get_env(&mut self, args: Value) -> Result<ToolCallResult> {
        let name = args["session"].as_str().unwrap_or("");
        let key = args.get("key").and_then(|v| v.as_str());
        let cmd = if let Some(k) = key {
            format!("echo ${}", k)
        } else {
            "env".to_string()
        };
        let cmd_args = json!({"session": name, "command": cmd});
        self.session_run(cmd_args).await
    }

    async fn session_list(&self, _args: Value) -> Result<ToolCallResult> {
        let list: Vec<Value> = self
            .sessions
            .iter()
            .map(|(name, s)| {
                json!({
                    "name": name,
                    "cwd": s.cwd,
                    "created_at": s.created_at,
                    "history_count": s.history.len(),
                })
            })
            .collect();
        Ok(text_result(serde_json::to_string_pretty(&list)?))
    }

    async fn session_history(&self, args: Value) -> Result<ToolCallResult> {
        let name = args["session"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(20) as usize;
        let session = match self.sessions.get(name) {
            Some(s) => s,
            None => return Ok(error_result(format!("session '{}' not found", name))),
        };
        let history: Vec<Value> = session
            .history
            .iter()
            .rev()
            .take(limit)
            .rev()
            .enumerate()
            .map(|(i, (cmd, out))| {
                json!({"index": i, "command": cmd, "output_preview": &out[..out.len().min(200)]})
            })
            .collect();
        Ok(text_result(serde_json::to_string_pretty(&history)?))
    }

    // ═══════════════════════════════════════════
    // TRANSFORMS
    // ═══════════════════════════════════════════

    async fn transform_grep(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or("");
        let pattern = args["pattern"].as_str().unwrap_or("");
        let context = args["context"].as_u64().unwrap_or(0);
        let recursive = args["recursive"].as_bool().unwrap_or(false);
        if path.is_empty() || pattern.is_empty() {
            return Ok(error_result("path and pattern are required"));
        }
        let mut cmd = format!("grep -n");
        if context > 0 {
            cmd.push_str(&format!(" -C {}", context));
        }
        if recursive {
            cmd.push_str(" -r");
        }
        cmd.push_str(&format!(" -E '{}' '{}'", pattern, path));
        run_bash(&cmd, None, 30).await
    }

    async fn transform_find_replace(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or("");
        let find = args["find"].as_str().unwrap_or("");
        let replace = args["replace"].as_str().unwrap_or("");
        let is_regex = args["regex"].as_bool().unwrap_or(false);
        if path.is_empty() || find.is_empty() {
            return Ok(error_result("path and find are required"));
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return Ok(error_result(format!("read error: {}", e))),
        };

        let new_content = if is_regex {
            match regex::Regex::new(find) {
                Ok(re) => re.replace_all(&content, replace).to_string(),
                Err(e) => return Ok(error_result(format!("invalid regex: {}", e))),
            }
        } else {
            content.replace(find, replace)
        };

        let changes = if content == new_content { 0 } else { 1 };
        if changes > 0 {
            std::fs::write(path, &new_content)?;
        }
        Ok(text_result(format!(
            "{}: {} replacement(s) applied",
            path, changes
        )))
    }

    async fn transform_diff_files(&self, args: Value) -> Result<ToolCallResult> {
        let file_a = args["file_a"].as_str().unwrap_or("");
        let file_b = args["file_b"].as_str().unwrap_or("");
        if file_a.is_empty() || file_b.is_empty() {
            return Ok(error_result("file_a and file_b are required"));
        }
        let cmd = format!("diff -u '{}' '{}'", file_a, file_b);
        run_bash(&cmd, None, 15).await
    }

    async fn transform_extract_lines(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or("");
        let start = args["start"].as_u64().unwrap_or(1) as usize;
        let end = args["end"].as_u64().unwrap_or(u64::MAX) as usize;
        if path.is_empty() {
            return Ok(error_result("path is required"));
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return Ok(error_result(format!("read error: {}", e))),
        };
        let selected: String = content
            .lines()
            .enumerate()
            .filter(|(i, _)| *i + 1 >= start && *i + 1 <= end)
            .map(|(i, l)| format!("{:>5}\t{}", i + 1, l))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(text_result(selected))
    }

    async fn transform_json_format(&self, args: Value) -> Result<ToolCallResult> {
        let input = args["json_string"].as_str().unwrap_or("");
        if input.is_empty() {
            return Ok(error_result("json_string is required"));
        }
        match serde_json::from_str::<Value>(input) {
            Ok(v) => Ok(text_result(serde_json::to_string_pretty(&v)?)),
            Err(e) => Ok(error_result(format!("invalid JSON: {}", e))),
        }
    }

    async fn transform_hash_file(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or("");
        let algo = args["algorithm"].as_str().unwrap_or("sha256");
        if path.is_empty() {
            return Ok(error_result("path is required"));
        }
        let cmd = match algo {
            "sha256" => format!("sha256sum '{}'", path),
            "sha1" => format!("sha1sum '{}'", path),
            "md5" => format!("md5sum '{}'", path),
            _ => format!("sha256sum '{}'", path),
        };
        run_bash(&cmd, None, 15).await
    }

    async fn transform_file_stats(&self, args: Value) -> Result<ToolCallResult> {
        let path = args["path"].as_str().unwrap_or("");
        let recursive = args["recursive"].as_bool().unwrap_or(false);
        if path.is_empty() {
            return Ok(error_result("path is required"));
        }
        let cmd = if recursive {
            format!("find '{}' -type f | wc -l && du -sh '{}'", path, path)
        } else {
            format!("stat '{}'", path)
        };
        run_bash(&cmd, None, 15).await
    }

    // ═══════════════════════════════════════════
    // GIT
    // ═══════════════════════════════════════════

    async fn git_status(&self, args: Value) -> Result<ToolCallResult> {
        let repo = args["repo_path"].as_str().unwrap_or(".");
        run_bash(&format!("git -C '{}' status", repo), None, 15).await
    }

    async fn git_diff(&self, args: Value) -> Result<ToolCallResult> {
        let repo = args["repo_path"].as_str().unwrap_or(".");
        let file = args["file"].as_str();
        let staged = args["staged"].as_bool().unwrap_or(false);
        let mut cmd = format!("git -C '{}'", repo);
        if staged {
            cmd.push_str(" diff --cached");
        } else {
            cmd.push_str(" diff");
        }
        if let Some(f) = file {
            cmd.push_str(&format!(" -- '{}'", f));
        }
        run_bash(&cmd, None, 15).await
    }

    async fn git_commit(&self, args: Value) -> Result<ToolCallResult> {
        let repo = args["repo_path"].as_str().unwrap_or(".");
        let message = args["message"].as_str().unwrap_or("");
        let all = args["all"].as_bool().unwrap_or(false);
        if message.is_empty() {
            return Ok(error_result("message is required"));
        }

        if let Some(files) = args["files"].as_array() {
            for f in files {
                if let Some(path) = f.as_str() {
                    let _ = run_bash(
                        &format!("git -C '{}' add '{}'", repo, path),
                        None,
                        10,
                    )
                    .await;
                }
            }
        }

        let mut cmd = format!("git -C '{}' commit", repo);
        if all {
            cmd.push_str(" -a");
        }
        cmd.push_str(&format!(" -m '{}'", message.replace('\'', "'\\''")));
        run_bash(&cmd, None, 15).await
    }

    async fn git_log(&self, args: Value) -> Result<ToolCallResult> {
        let repo = args["repo_path"].as_str().unwrap_or(".");
        let limit = args["limit"].as_u64().unwrap_or(10);
        let oneline = args["oneline"].as_bool().unwrap_or(false);
        let mut cmd = format!("git -C '{}' log -n {}", repo, limit);
        if oneline {
            cmd.push_str(" --oneline");
        }
        run_bash(&cmd, None, 15).await
    }

    async fn git_stash(&self, args: Value) -> Result<ToolCallResult> {
        let repo = args["repo_path"].as_str().unwrap_or(".");
        let action = args["action"].as_str().unwrap_or("push");
        let message = args["message"].as_str();
        let mut cmd = format!("git -C '{}' stash {}", repo, action);
        if action == "push" {
            if let Some(m) = message {
                cmd.push_str(&format!(" -m '{}'", m));
            }
        }
        run_bash(&cmd, None, 15).await
    }

    async fn git_branch(&self, args: Value) -> Result<ToolCallResult> {
        let repo = args["repo_path"].as_str().unwrap_or(".");
        let action = args["action"].as_str().unwrap_or("list");
        let name = args["name"].as_str();
        let cmd = match action {
            "list" => format!("git -C '{}' branch -a", repo),
            "create" => {
                let n = name.unwrap_or("new-branch");
                format!("git -C '{}' branch '{}'", repo, n)
            }
            "delete" => {
                let n = name.unwrap_or("");
                format!("git -C '{}' branch -d '{}'", repo, n)
            }
            _ => format!("git -C '{}' branch -a", repo),
        };
        run_bash(&cmd, None, 15).await
    }

    async fn git_checkout(&self, args: Value) -> Result<ToolCallResult> {
        let repo = args["repo_path"].as_str().unwrap_or(".");
        let target = args["target"].as_str().unwrap_or("");
        let create = args["create"].as_bool().unwrap_or(false);
        if target.is_empty() {
            return Ok(error_result("target is required"));
        }
        let flag = if create { "-b" } else { "" };
        let cmd = format!("git -C '{}' checkout {} '{}'", repo, flag, target);
        run_bash(&cmd, None, 15).await
    }

    async fn git_reset(&self, args: Value) -> Result<ToolCallResult> {
        let repo = args["repo_path"].as_str().unwrap_or(".");
        let target = args["target"].as_str().unwrap_or("HEAD");
        let mode = args["mode"].as_str().unwrap_or("mixed");
        let cmd = format!("git -C '{}' reset --{} '{}'", repo, mode, target);
        run_bash(&cmd, None, 15).await
    }

    // ═══════════════════════════════════════════
    // HTTP
    // ═══════════════════════════════════════════

    async fn http_request(&self, args: Value) -> Result<ToolCallResult> {
        let url = args["url"].as_str().unwrap_or("");
        if url.is_empty() {
            return Ok(error_result("url is required"));
        }
        let method = args["method"].as_str().unwrap_or("GET").to_uppercase();
        let timeout = args["timeout_secs"].as_u64().unwrap_or(30);
        let headers: Vec<(&str, &str)> = args["headers"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|vs| (k.as_str(), vs)))
                    .collect()
            })
            .unwrap_or_default();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout))
            .build()?;

        let mut req = match method.as_str() {
            "POST" => client.post(url),
            "PUT" => client.put(url),
            "PATCH" => client.patch(url),
            "DELETE" => client.delete(url),
            "HEAD" => client.head(url),
            _ => client.get(url),
        };

        for (k, v) in headers {
            req = req.header(k, v);
        }

        if let Some(body) = args["body"].as_str() {
            req = req.body(body.to_string());
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                let truncated = if body.len() > 50000 {
                    format!("{}... [truncated at 50KB]", &body[..50000])
                } else {
                    body
                };
                Ok(text_result(format!(
                    "HTTP {} {}\n\n{}",
                    status,
                    if status >= 200 && status < 300 { "OK" } else { "ERROR" },
                    truncated
                )))
            }
            Err(e) => Ok(error_result(format!("request failed: {}", e))),
        }
    }

    async fn http_fetch(&self, args: Value) -> Result<ToolCallResult> {
        let url = args["url"].as_str().unwrap_or("");
        if url.is_empty() {
            return Ok(error_result("url is required"));
        }
        self.http_request(json!({"url": url, "method": "GET", "timeout_secs": 30}))
            .await
    }

    async fn port_check(&self, args: Value) -> Result<ToolCallResult> {
        let host = args["host"].as_str().unwrap_or("127.0.0.1");
        let port = args["port"].as_u64().unwrap_or(0) as u16;
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(2000);
        if port == 0 {
            return Ok(error_result("port is required"));
        }

        let addr = format!("{}:{}", host, port);
        let timeout = std::time::Duration::from_millis(timeout_ms);

        match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&addr)).await {
            Ok(Ok(_)) => Ok(text_result(format!("{}:{} is OPEN", host, port))),
            Ok(Err(e)) => Ok(text_result(format!("{}:{} is CLOSED ({})", host, port, e))),
            Err(_) => Ok(text_result(format!("{}:{} TIMEOUT after {}ms", host, port, timeout_ms))),
        }
    }

    // ═══════════════════════════════════════════
    // SYSTEM
    // ═══════════════════════════════════════════

    async fn system_info(&self, _args: Value) -> Result<ToolCallResult> {
        run_bash(
            "echo '=== OS ===' && uname -a && echo '=== Memory ===' && free -h && echo '=== Disk ===' && df -h / && echo '=== CPU ===' && nproc && echo '=== Uptime ===' && uptime",
            None,
            15,
        )
        .await
    }

    async fn kill_process(&self, args: Value) -> Result<ToolCallResult> {
        let pid = args["pid"].as_u64().unwrap_or(0);
        if pid == 0 {
            return Ok(error_result("pid is required"));
        }
        run_bash(&format!("kill {}", pid), None, 5).await
    }

    async fn list_process(&self, args: Value) -> Result<ToolCallResult> {
        let filter = args["filter"].as_str();
        let cmd = if let Some(f) = filter {
            format!("ps aux | head -1 && ps aux | grep -i '{}' | grep -v grep", f)
        } else {
            "ps aux --sort=-%mem | head -30".to_string()
        };
        run_bash(&cmd, None, 10).await
    }
}

// ── Tool Definitions ──

fn filesystem_tools() -> Vec<McpTool> {
    vec![
        tool("read_file", "Read a file with optional line ranges, in-file search, and auto-truncation", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path to read"},
                "lines": {"type": "string", "description": "Line range e.g. '10-50'"},
                "search": {"type": "string", "description": "Search term to filter lines"},
                "max_kb": {"type": "integer", "description": "Max file size in KB (default 100)"}
            },
            "required": ["path"]
        })),
        tool("write_file", "Write content to a file (creates parent dirs)", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path"},
                "content": {"type": "string", "description": "Content to write"}
            },
            "required": ["path", "content"]
        })),
        tool("append_file", "Append content to a file", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path"},
                "content": {"type": "string", "description": "Content to append"}
            },
            "required": ["path", "content"]
        })),
        tool("list_dir", "List directory contents as a tree", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory path (default '.')"},
                "depth": {"type": "integer", "description": "Max depth (default 2)"}
            },
            "required": ["path"]
        })),
        tool("archive_create", "Create a .tar.gz archive", json!({
            "type": "object",
            "properties": {
                "output": {"type": "string", "description": "Output archive path"},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Paths to include"}
            },
            "required": ["output", "paths"]
        })),
        tool("archive_extract", "Extract a .tar.gz archive", json!({
            "type": "object",
            "properties": {
                "archive": {"type": "string", "description": "Archive path"},
                "destination": {"type": "string", "description": "Extraction destination (default '.')"}
            },
            "required": ["archive"]
        })),
    ]
}

fn shell_tools() -> Vec<McpTool> {
    vec![
        tool("bash", "Execute a bash command with three-tier safety: Tier-1 free, Tier-2 needs confirm:true, Tier-3 needs allow_destructive:true. Catastrophic patterns blocked unconditionally.", json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Bash command to execute"},
                "working_dir": {"type": "string", "description": "Working directory"},
                "timeout_secs": {"type": "integer", "description": "Timeout in seconds (default 120)"},
                "confirm": {"type": "boolean", "description": "Confirm Tier-2 operations"},
                "allow_destructive": {"type": "boolean", "description": "Allow Tier-3 destructive operations"}
            },
            "required": ["command"]
        })),
        tool("run", "Simple bash command wrapper, stdout-only, no tier checks", json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Command to run"}
            },
            "required": ["command"]
        })),
    ]
}

fn session_tools() -> Vec<McpTool> {
    vec![
        tool("session_create", "Create a persistent bash session", json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Session name"},
                "cwd": {"type": "string", "description": "Working directory (default /root)"}
            }
        })),
        tool("session_run", "Run a command in a persistent session", json!({
            "type": "object",
            "properties": {
                "session": {"type": "string", "description": "Session name"},
                "command": {"type": "string", "description": "Command to run"},
                "confirm": {"type": "boolean"},
                "allow_destructive": {"type": "boolean"}
            },
            "required": ["session", "command"]
        })),
        tool("session_destroy", "Destroy a persistent session", json!({
            "type": "object",
            "properties": {"session": {"type": "string"}},
            "required": ["session"]
        })),
        tool("session_cd", "Change directory in a session", json!({
            "type": "object",
            "properties": {
                "session": {"type": "string"},
                "path": {"type": "string"}
            },
            "required": ["session", "path"]
        })),
        tool("session_set_env", "Set environment variable in a session", json!({
            "type": "object",
            "properties": {
                "session": {"type": "string"},
                "key": {"type": "string"},
                "value": {"type": "string"}
            },
            "required": ["session", "key", "value"]
        })),
        tool("session_get_env", "Get environment variable(s) from a session", json!({
            "type": "object",
            "properties": {
                "session": {"type": "string"},
                "key": {"type": "string", "description": "Specific var (omit for all)"}
            },
            "required": ["session"]
        })),
        tool("session_list", "List all active sessions", json!({
            "type": "object",
            "properties": {}
        })),
        tool("session_history", "Get command history for a session", json!({
            "type": "object",
            "properties": {
                "session": {"type": "string"},
                "limit": {"type": "integer", "description": "Max entries (default 20)"}
            },
            "required": ["session"]
        })),
    ]
}

fn transform_tools() -> Vec<McpTool> {
    vec![
        tool("transform_grep", "Search file contents with regex", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "pattern": {"type": "string"},
                "context": {"type": "integer", "description": "Context lines around matches"},
                "recursive": {"type": "boolean"}
            },
            "required": ["path", "pattern"]
        })),
        tool("transform_find_replace", "Find and replace in a file", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "find": {"type": "string"},
                "replace": {"type": "string"},
                "regex": {"type": "boolean", "description": "Treat find as regex"}
            },
            "required": ["path", "find", "replace"]
        })),
        tool("transform_diff_files", "Diff two files", json!({
            "type": "object",
            "properties": {
                "file_a": {"type": "string"},
                "file_b": {"type": "string"}
            },
            "required": ["file_a", "file_b"]
        })),
        tool("transform_extract_lines", "Extract a range of lines from a file", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "start": {"type": "integer"},
                "end": {"type": "integer"}
            },
            "required": ["path", "start", "end"]
        })),
        tool("transform_json_format", "Pretty-print a JSON string", json!({
            "type": "object",
            "properties": {"json_string": {"type": "string"}},
            "required": ["json_string"]
        })),
        tool("transform_hash_file", "Compute hash of a file", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "algorithm": {"type": "string", "description": "sha256 (default), sha1, or md5"}
            },
            "required": ["path"]
        })),
        tool("transform_file_stats", "Get file/directory statistics", json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "recursive": {"type": "boolean"}
            },
            "required": ["path"]
        })),
    ]
}

fn git_tools() -> Vec<McpTool> {
    vec![
        tool("git_status", "Show git status", json!({"type":"object","properties":{"repo_path":{"type":"string"}},"required":["repo_path"]})),
        tool("git_diff", "Show git diff", json!({"type":"object","properties":{"repo_path":{"type":"string"},"file":{"type":"string"},"staged":{"type":"boolean"}},"required":["repo_path"]})),
        tool("git_commit", "Create a git commit", json!({"type":"object","properties":{"repo_path":{"type":"string"},"message":{"type":"string"},"files":{"type":"array","items":{"type":"string"}},"all":{"type":"boolean"}},"required":["repo_path","message"]})),
        tool("git_log", "Show git log", json!({"type":"object","properties":{"repo_path":{"type":"string"},"limit":{"type":"integer"},"oneline":{"type":"boolean"}},"required":["repo_path"]})),
        tool("git_stash", "Git stash operations", json!({"type":"object","properties":{"repo_path":{"type":"string"},"action":{"type":"string","description":"push, pop, list, drop"},"message":{"type":"string"}},"required":["repo_path","action"]})),
        tool("git_branch", "Git branch operations", json!({"type":"object","properties":{"repo_path":{"type":"string"},"action":{"type":"string","description":"list, create, delete"},"name":{"type":"string"}},"required":["repo_path"]})),
        tool("git_checkout", "Git checkout", json!({"type":"object","properties":{"repo_path":{"type":"string"},"target":{"type":"string"},"create":{"type":"boolean"}},"required":["repo_path","target"]})),
        tool("git_reset", "Git reset", json!({"type":"object","properties":{"repo_path":{"type":"string"},"target":{"type":"string"},"mode":{"type":"string","description":"soft, mixed, hard"}},"required":["repo_path"]})),
    ]
}

fn http_tools() -> Vec<McpTool> {
    vec![
        tool("http_request", "Make an HTTP request", json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "method": {"type": "string", "description": "GET, POST, PUT, PATCH, DELETE, HEAD"},
                "headers": {"type": "object", "additionalProperties": {"type": "string"}},
                "body": {"type": "string"},
                "timeout_secs": {"type": "integer"}
            },
            "required": ["url"]
        })),
        tool("http_fetch", "Simple HTTP GET", json!({
            "type": "object",
            "properties": {"url": {"type": "string"}},
            "required": ["url"]
        })),
        tool("port_check", "Check if a port is open", json!({
            "type": "object",
            "properties": {
                "host": {"type": "string", "description": "Host (default 127.0.0.1)"},
                "port": {"type": "integer"},
                "timeout_ms": {"type": "integer", "description": "Timeout in ms (default 2000)"}
            },
            "required": ["port"]
        })),
    ]
}

fn system_tools() -> Vec<McpTool> {
    vec![
        tool("system_info", "Get system information (OS, memory, disk, CPU, uptime)", json!({"type":"object","properties":{}})),
        tool("kill_process", "Kill a process by PID", json!({"type":"object","properties":{"pid":{"type":"integer"}},"required":["pid"]})),
        tool("list_process", "List running processes", json!({"type":"object","properties":{"filter":{"type":"string","description":"Filter by name"}}})),
    ]
}

fn breadcrumb_tools() -> Vec<McpTool> {
    vec![
        tool("breadcrumb_start", "Start a tracked multi-step operation", json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Operation name"},
                "steps": {"type": "array", "items": {"type": "string"}, "description": "Planned step names"},
                "project_id": {"type": "string", "description": "Optional project grouping"}
            },
            "required": ["name", "steps"]
        })),
        tool("breadcrumb_step", "Log progress on current step", json!({
            "type": "object",
            "properties": {
                "result": {"type": "string", "description": "Step result/summary"},
                "files_changed": {"type": "array", "items": {"type": "string"}},
                "breadcrumb_id": {"type": "string"}
            },
            "required": ["result"]
        })),
        tool("breadcrumb_complete", "Complete a breadcrumb operation", json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"},
                "breadcrumb_id": {"type": "string"}
            }
        })),
        tool("breadcrumb_abort", "Abort a breadcrumb operation", json!({
            "type": "object",
            "properties": {
                "reason": {"type": "string"},
                "breadcrumb_id": {"type": "string"}
            },
            "required": ["reason"]
        })),
        tool("breadcrumb_status", "Get breadcrumb status", json!({
            "type": "object",
            "properties": {}
        })),
        tool("breadcrumb_list", "List breadcrumbs", json!({
            "type": "object",
            "properties": {
                "scope": {"type": "string", "description": "active, today, week, all (default active)"}
            }
        })),
        tool("breadcrumb_adopt", "Adopt/reassign a breadcrumb", json!({
            "type": "object",
            "properties": {"breadcrumb_id": {"type": "string"}},
            "required": ["breadcrumb_id"]
        })),
    ]
}

fn loaf_tools() -> Vec<McpTool> {
    vec![
        tool("loaf_create", "Create a new project loaf for multi-task tracking", json!({
            "type": "object",
            "properties": {
                "project_name": {"type": "string"},
                "goal": {"type": "string"},
                "phases": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["project_name", "goal"]
        })),
        tool("loaf_update", "Update a loaf with task progress, decisions, or discoveries", json!({
            "type": "object",
            "properties": {
                "loaf_id": {"type": "string"},
                "task_update": {"type": "object", "properties": {
                    "task_id": {"type": "string"},
                    "status": {"type": "string"},
                    "output_summary": {"type": "string"},
                    "files_changed": {"type": "array", "items": {"type": "string"}},
                    "decisions_made": {"type": "array", "items": {"type": "string"}},
                    "discoveries": {"type": "array", "items": {"type": "string"}}
                }},
                "decision": {"type": "object", "properties": {
                    "what": {"type": "string"},
                    "why": {"type": "string"},
                    "who": {"type": "string"}
                }},
                "discovery": {"type": "object", "properties": {
                    "what": {"type": "string"},
                    "impact": {"type": "string"}
                }},
                "next_actions": {"type": "array", "items": {"type": "string"}},
                "phase_status": {"type": "string"}
            },
            "required": ["loaf_id"]
        })),
        tool("loaf_status", "Get loaf status", json!({
            "type": "object",
            "properties": {"loaf_id": {"type": "string"}}
        })),
        tool("loaf_close", "Close/archive a loaf", json!({
            "type": "object",
            "properties": {"loaf_id": {"type": "string"}},
            "required": ["loaf_id"]
        })),
    ]
}

// ── Shared bash execution ──

async fn run_bash(command: &str, working_dir: Option<&str>, timeout_secs: u64) -> Result<ToolCallResult> {
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c").arg(command);
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let timeout = std::time::Duration::from_secs(timeout_secs);

    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut result = stdout.to_string();
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str("[stderr] ");
                result.push_str(&stderr);
            }
            if result.len() > 100_000 {
                result.truncate(100_000);
                result.push_str("\n... [truncated at 100KB]");
            }
            if output.status.success() {
                Ok(text_result(result))
            } else {
                Ok(error_result(format!(
                    "exit code {}\n{}",
                    output.status.code().unwrap_or(-1),
                    result
                )))
            }
        }
        Ok(Err(e)) => Ok(error_result(format!("spawn error: {}", e))),
        Err(_) => Ok(error_result(format!(
            "command timed out after {}s",
            timeout_secs
        ))),
    }
}
