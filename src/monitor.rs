use crate::model::*;
use chrono::DateTime;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

const MAX_LINE: usize = 8 * 1024 * 1024;
#[derive(Default)]
pub struct SessionReader {
    pub session: Option<Session>,
    pub usage: Option<Usage>,
    offset: u64,
    initialized: bool,
    pending: Vec<u8>,
    skip: bool,
}
impl SessionReader {
    pub fn read(&mut self, path: &Path) -> io::Result<()> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        if self.initialized && len < self.offset {
            *self = Self::default();
        }
        if !self.initialized {
            let mut header = vec![0; len.min(65536) as usize];
            let count = file.read(&mut header)?;
            if let Some(end) = header[..count].iter().position(|b| *b == b'\n') {
                self.apply(&header[..end]);
            }
            self.offset = len.saturating_sub(1024 * 1024);
            self.skip = self.offset > 0;
            self.initialized = true;
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buffer = [0; 32768];
        let mut left = (len - self.offset).min(4 * 1024 * 1024);
        while left > 0 {
            let cap = buffer.len().min(left as usize);
            let count = file.read(&mut buffer[..cap])?;
            if count == 0 {
                break;
            }
            self.feed(&buffer[..count]);
            self.offset += count as u64;
            left -= count as u64;
        }
        Ok(())
    }
    pub fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let end = bytes.iter().position(|b| *b == b'\n');
            let segment = &bytes[..end.unwrap_or(bytes.len())];
            if !self.skip {
                if self.pending.len() + segment.len() > MAX_LINE {
                    self.pending.clear();
                    self.skip = true;
                } else {
                    self.pending.extend_from_slice(segment);
                }
            }
            let Some(end) = end else { break };
            if !self.skip {
                let line = std::mem::take(&mut self.pending);
                self.apply(&line);
                self.pending = line;
                self.pending.clear();
            }
            self.pending.clear();
            self.skip = false;
            bytes = &bytes[end + 1..];
        }
    }
    fn apply(&mut self, line: &[u8]) {
        let Ok(root) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let p = &root["payload"];
        let typ = root["type"].as_str().unwrap_or("");
        let kind = p["type"].as_str().unwrap_or("");
        let stamp = root["timestamp"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp())
            .unwrap_or(0);
        if typ == "session_meta" {
            self.session = Some(Session {
                session_id: p["id"]
                    .as_str()
                    .or(p["session_id"].as_str())
                    .unwrap_or("codex")
                    .into(),
                project: project(p["cwd"].as_str().unwrap_or("Codex")),
                state: "idle".into(),
                label: "Ожидает запроса".into(),
                updated_at: stamp,
                ..Default::default()
            });
            return;
        }
        if typ == "event_msg"
            && kind == "token_count"
            && p["rate_limits"].is_object()
            && p["rate_limits"]["limit_id"]
                .as_str()
                .is_none_or(|id| id == "codex")
            && stamp > 0
        {
            let parsed = parse_limits(&p["rate_limits"], stamp);
            if self
                .usage
                .as_ref()
                .is_none_or(|u| parsed.updated_at >= u.updated_at)
            {
                self.usage = Some(parsed);
            }
        }
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if s.title.is_none() {
            let msg = if kind == "item_completed" {
                &p["item"]
            } else {
                p
            };
            let mut title = if typ == "event_msg" && kind == "user_message" {
                p["message"].as_str().and_then(task_name)
            } else {
                None
            };
            if (typ == "response_item" && p["role"] == "user") || msg["type"] == "UserMessage" {
                if let Some(parts) = msg["content"].as_array() {
                    title = parts
                        .iter()
                        .filter_map(|c| c["text"].as_str().and_then(task_name))
                        .next();
                }
            }
            s.title = title;
        }
        if stamp < s.updated_at {
            return;
        }
        let action = match (typ, kind) {
            ("event_msg", "task_started" | "turn_started") => {
                Some(("thinking", "Обдумывает ответ"))
            }
            ("event_msg", "task_complete" | "turn_complete") => Some(("done", "Ответ готов")),
            ("event_msg", "turn_aborted") => Some(("done", "Ответ прерван")),
            ("event_msg", "approval_request" | "permission_request") => {
                Some(("permission", "Ожидает разрешения"))
            }
            ("event_msg", "token_count") if s.started_at > 0 => {
                Some(("thinking", "Обдумывает ответ"))
            }
            ("response_item", "function_call" | "custom_tool_call") => {
                Some(("tool", tool_label(p["name"].as_str().unwrap_or(""))))
            }
            ("response_item", "function_call_output" | "custom_tool_call_output") => {
                Some(("thinking", "Обдумывает ответ"))
            }
            _ => None,
        };
        if let Some((state, label)) = action {
            s.state = state.into();
            s.label = label.into();
            s.updated_at = stamp;
            s.started_at = if state == "done" {
                0
            } else if matches!(kind, "task_started" | "turn_started") || s.started_at == 0 {
                stamp
            } else {
                s.started_at
            };
        }
    }
}
pub fn tool_label(name: &str) -> &'static str {
    match name {
        "exec_command" | "shell_command" | "shell" | "Bash" | "PowerShell" => "Выполняет команду",
        "apply_patch" | "Edit" | "Write" => "Редактирует файлы",
        "Read" => "Читает файл",
        "Grep" | "Glob" => "Ищет в файлах",
        "web.run" | "web__run" | "WebSearch" => "Ищет в интернете",
        "spawn_agent" | "Agent" | "Task" => "Выполняет подзадачу",
        _ => "Использует инструмент",
    }
}
pub fn parse_limits(value: &Value, stamp: i64) -> Usage {
    let mut usage = Usage {
        five_hour: None,
        seven_day: None,
        updated_at: DateTime::from_timestamp(stamp, 0).unwrap_or_default(),
    };
    for key in ["primary", "secondary"] {
        let w = &value[key];
        let (Some(used), Some(reset), Some(minutes)) = (
            w["used_percent"].as_f64(),
            w["resets_at"].as_i64(),
            w["window_minutes"].as_i64(),
        ) else {
            continue;
        };
        if !used.is_finite() || used < 0. || reset <= 0 {
            continue;
        }
        let limit = Some(Limit {
            used,
            resets_at: reset,
        });
        match minutes {
            300 => usage.five_hour = limit,
            10080 => usage.seven_day = limit,
            _ => {}
        }
    }
    usage
}
pub struct Monitor {
    directory: PathBuf,
    index: PathBuf,
    index_stamp: Option<SystemTime>,
    titles: HashMap<String, String>,
    readers: HashMap<PathBuf, SessionReader>,
    discovered: Instant,
    last_usage: Option<Usage>,
}
impl Monitor {
    pub fn new(home: PathBuf) -> Self {
        Self {
            directory: home.join("sessions"),
            index: home.join("session_index.jsonl"),
            index_stamp: None,
            titles: HashMap::new(),
            readers: HashMap::new(),
            discovered: Instant::now() - std::time::Duration::from_secs(60),
            last_usage: None,
        }
    }
    pub fn poll(&mut self) -> Provider {
        if self.discovered.elapsed().as_secs() >= 30 {
            let mut files = Vec::new();
            collect(&self.directory, &mut files);
            files.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
            for (file, _) in files.into_iter().take(16) {
                self.readers.entry(file).or_default();
            }
            self.discovered = Instant::now();
        }
        self.load_titles();
        let n = now();
        let mut sessions = Vec::new();
        for (path, reader) in &mut self.readers {
            let _ = reader.read(path);
            if let Some(u) = &reader.usage {
                if self
                    .last_usage
                    .as_ref()
                    .is_none_or(|old| u.updated_at > old.updated_at)
                {
                    self.last_usage = Some(u.clone());
                }
            }
            if let Some(s) = &reader.session {
                if n - s.updated_at > 43200 || s.updated_at > n + 60 {
                    continue;
                }
                let mut s = s.clone();
                if let Some(title) = self.titles.get(&s.session_id) {
                    s.title = Some(title.clone());
                }
                if s.priority() > 0 && n - s.updated_at > 900 {
                    s.state = "idle".into();
                    s.label = "Нет свежих событий".into();
                    s.started_at = 0;
                }
                sessions.push(s);
            }
        }
        self.readers
            .retain(|_, r| r.session.as_ref().is_none_or(|s| n - s.updated_at < 86400));
        sort_sessions(&mut sessions);
        Provider {
            name: "Codex".into(),
            sessions,
            usage: self.last_usage.clone(),
            notice: self
                .last_usage
                .is_none()
                .then(|| "Лимиты появятся после ответа Codex".into()),
        }
    }
    fn load_titles(&mut self) {
        let stamp = fs::metadata(&self.index).and_then(|m| m.modified()).ok();
        if stamp == self.index_stamp {
            return;
        }
        if let Ok(bytes) = fs::read(&self.index) {
            for line in bytes.split(|b| *b == b'\n') {
                if let Ok(v) = serde_json::from_slice::<Value>(line) {
                    if let (Some(id), Some(title)) = (
                        v["id"].as_str(),
                        v["thread_name"].as_str().and_then(task_name),
                    ) {
                        self.titles.insert(id.into(), title);
                    }
                }
            }
            self.index_stamp = stamp;
        }
    }
}
fn collect(dir: &Path, files: &mut Vec<(PathBuf, SystemTime)>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                collect(&path, files);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                if let Ok(stamp) = entry.metadata().and_then(|m| m.modified()) {
                    files.push((path, stamp));
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use serde_json::json;
    fn event(typ: &str, payload: Value) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(
            &json!({"timestamp": Utc::now().to_rfc3339(), "type":typ,"payload":payload}),
        )
        .unwrap();
        bytes.push(b'\n');
        bytes
    }
    #[test]
    fn partial_utf8_lifecycle_and_title() {
        let mut r = SessionReader::default();
        let bytes = event(
            "session_meta",
            json!({"id":"one","cwd":"G:\\Проекты\\пример"}),
        );
        for chunk in bytes.chunks(7) {
            r.feed(chunk)
        }
        assert_eq!(r.session.as_ref().unwrap().project, "пример");
        r.feed(&event(
            "event_msg",
            json!({"type":"user_message","message":"# AGENTS.md instructions"}),
        ));
        assert!(r.session.as_ref().unwrap().title.is_none());
        r.feed(&event(
            "event_msg",
            json!({"type":"user_message","message":"Добавить настройки внешнего вида"}),
        ));
        assert!(r.session.as_ref().unwrap().title.is_some());
        r.feed(&event("event_msg", json!({"type":"task_started"})));
        let start = r.session.as_ref().unwrap().started_at;
        assert!(start > 0);
        r.feed(&event(
            "response_item",
            json!({"type":"function_call","name":"apply_patch"}),
        ));
        assert_eq!(r.session.as_ref().unwrap().state, "tool");
        let bytes = event("event_msg", json!({"type":"task_complete"}));
        r.feed(&bytes[..bytes.len() - 1]);
        assert_eq!(r.session.as_ref().unwrap().state, "tool");
        r.feed(b"\n");
        assert_eq!(r.session.as_ref().unwrap().started_at, 0);
    }
    #[test]
    fn windows_by_duration_and_missing_data() {
        let u = parse_limits(
            &json!({"primary":{"used_percent":0,"resets_at":2000000000,"window_minutes":10080},"secondary":{"used_percent":45.5,"resets_at":2000000000,"window_minutes":300}}),
            100,
        );
        assert_eq!(u.five_hour.unwrap().used, 45.5);
        assert_eq!(u.seven_day.unwrap().used, 0.);
        let u = parse_limits(
            &json!({"primary":{"used_percent":"bad","resets_at":2,"window_minutes":300}}),
            100,
        );
        assert!(u.five_hour.is_none());
    }
}
