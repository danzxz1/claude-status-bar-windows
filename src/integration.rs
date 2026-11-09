use crate::{model::*, monitor::tool_label};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Diagnostics::ToolHelp::*, Threading::*},
};
const MARKER: &str = "--cwsb-managed";
const EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
];
const NO_WINDOW: u32 = 0x08000000;
#[derive(Clone)]
pub struct Context {
    pub root: PathBuf,
    pub settings: PathBuf,
    pub exe: PathBuf,
    pub test: bool,
}
impl Context {
    pub fn current() -> io::Result<Self> {
        Ok(Self {
            root: data_dir(),
            settings: std::env::var_os("CWSB_SETTINGS_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".claude/settings.json")),
            exe: std::env::current_exe()?,
            test: std::env::var_os("CWSB_TEST_MODE").is_some(),
        })
    }
    fn config(&self) -> PathBuf {
        self.root.join("integration.json")
    }
    pub fn install(&self) -> Result<(), String> {
        let mut settings: Value = if self.settings.exists() {
            read_json(&self.settings).ok_or("Некорректный settings.json. Файл не изменён.")?
        } else {
            json!({})
        };
        if !settings.is_object() {
            return Err("settings.json должен быть объектом".into());
        }
        if settings
            .get("statusLine")
            .is_some_and(|s| s["type"] != "command")
        {
            return Err("Неподдерживаемый тип statusLine".into());
        }
        let managed = settings["statusLine"]["command"]
            .as_str()
            .is_some_and(|s| s.contains(MARKER));
        let previous: Value = read_json(&self.config()).unwrap_or(json!({}));
        if managed && !self.config().exists() {
            return Err(
                "Отсутствует integration.json. Восстановите настройки из резервной копии.".into(),
            );
        }
        let original = if managed {
            previous["originalStatusLine"].clone()
        } else {
            settings["statusLine"].clone()
        };
        let bash = find_bash();
        if original["command"].is_string() && bash.is_none() {
            return Err(
                "Не найден Git Bash для исходной statusLine. Задайте CLAUDE_CODE_GIT_BASH_PATH."
                    .into(),
            );
        }
        self.backup("settings-backup")?;
        let command = quote(&self.exe.to_string_lossy().replace('\\', "/"));
        if settings.get("hooks").is_none() {
            settings["hooks"] = json!({});
        }
        if !settings["hooks"].is_object() {
            return Err("Неподдерживаемый формат hooks".into());
        }
        for event in EVENTS {
            let mut entries = clean_entries(&settings["hooks"][event]);
            let mut entry = json!({"hooks":[{"type":"command","command":format!("{command} --hook {event} {MARKER}"),"timeout":5}]});
            if [
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "PermissionRequest",
            ]
            .contains(event)
            {
                entry["matcher"] = json!("*");
            }
            entries.push(entry);
            settings["hooks"][event] = json!(entries);
        }
        settings["statusLine"] = if original.is_object() {
            original.clone()
        } else {
            json!({})
        };
        settings["statusLine"]["type"] = json!("command");
        settings["statusLine"]["command"] = json!(format!("{command} --statusline {MARKER}"));
        write_json(
            &self.config(),
            &json!({"exe":self.exe,"bash":bash,"originalStatusLine":original}),
        )
        .map_err(|e| e.to_string())?;
        write_json(&self.settings, &settings).map_err(|e| e.to_string())
    }
    pub fn uninstall(&self) -> Result<(), String> {
        let mut settings: Value =
            read_json(&self.settings).ok_or("Не удалось прочитать настройки")?;
        let managed = settings["statusLine"]["command"]
            .as_str()
            .is_some_and(|c| c.contains(MARKER));
        let config: Value = read_json(&self.config()).unwrap_or(json!({}));
        if managed && !self.config().exists() {
            return Err("Нет integration.json: исходную statusLine восстановить невозможно".into());
        }
        self.backup("settings-before-uninstall")?;
        for event in EVENTS {
            if settings["hooks"].get(*event).is_none() {
                continue;
            }
            let entries = clean_entries(&settings["hooks"][event]);
            if entries.is_empty() {
                settings["hooks"].as_object_mut().unwrap().remove(*event);
            } else {
                settings["hooks"][event] = json!(entries);
            }
        }
        if managed {
            if config["originalStatusLine"].is_null() {
                settings.as_object_mut().unwrap().remove("statusLine");
            } else {
                settings["statusLine"] = config["originalStatusLine"].clone();
            }
        }
        write_json(&self.settings, &settings).map_err(|e| e.to_string())
    }
    fn backup(&self, prefix: &str) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        if self.settings.exists() {
            fs::copy(
                &self.settings,
                self.root.join(format!(
                    "{prefix}-{}.json",
                    Utc::now().timestamp_nanos_opt().unwrap_or(0)
                )),
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn hook(&self, event: &str, p: &Value) -> io::Result<()> {
        let Some(id) = p["session_id"].as_str() else {
            return Ok(());
        };
        let hash = format!("{:x}", Sha256::digest(id.as_bytes()));
        let file = self.root.join("sessions").join(format!("{hash}.json"));
        if event == "SessionEnd" {
            if file.exists() {
                fs::remove_file(file)?;
            }
            return Ok(());
        }
        if event == "Notification" && p["notification_type"] != "permission_prompt" {
            return Ok(());
        }
        let old: Option<Session> = read_json(&file);
        let first = old.is_none();
        let mut s = old.unwrap_or_default();
        let action = match event {
            "SessionStart" => ("idle", "Ожидает запроса"),
            "UserPromptSubmit" => ("thinking", "Обдумывает ответ"),
            "PreToolUse" => ("tool", tool_label(p["tool_name"].as_str().unwrap_or(""))),
            "PostToolUse" | "PostToolUseFailure" => ("thinking", "Обдумывает ответ"),
            "PermissionRequest" | "Notification" => ("permission", "Ожидает разрешения"),
            "Stop" => ("done", "Ответ готов"),
            "StopFailure" => ("done", "Ответ прерван"),
            _ => return Ok(()),
        };
        if (event == "SessionStart" || first) && !self.test {
            if let Some((pid, start)) = find_claude() {
                s.pid = pid;
                s.process_started_at = start;
            }
        }
        s.session_id = id.into();
        if let Some(cwd) = p["cwd"].as_str() {
            s.project = project(cwd);
        }
        if let Some(title) = p["session_name"].as_str().and_then(task_name) {
            s.title = Some(title);
        }
        s.state = action.0.into();
        s.label = action.1.into();
        s.updated_at = now();
        s.started_at = if event == "UserPromptSubmit" {
            now()
        } else if matches!(action.0, "thinking" | "tool" | "permission") {
            if s.started_at > 0 {
                s.started_at
            } else {
                now()
            }
        } else {
            0
        };
        write_json(&file, &s)?;
        if (event == "SessionStart" || first) && !self.test {
            let _ = Command::new(&self.exe)
                .creation_flags(0x00000008 | NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
        Ok(())
    }
    pub fn capture_usage(&self, p: &Value) -> io::Result<()> {
        let Some(limits) = p.get("rate_limits") else {
            return Ok(());
        };
        let parse = |v: &Value| -> Option<Limit> {
            Some(Limit {
                used: v["used_percentage"]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.)?,
                resets_at: v["resets_at"].as_i64()?,
            })
        };
        write_json(
            &self.root.join("usage.json"),
            &Usage {
                five_hour: parse(&limits["five_hour"]),
                seven_day: parse(&limits["seven_day"]),
                updated_at: Utc::now(),
            },
        )
    }
    pub fn statusline(&self, raw: &[u8]) -> i32 {
        if let Ok(p) = serde_json::from_slice(raw) {
            let _ = self.capture_usage(&p);
        }
        let config: Value = read_json(&self.config()).unwrap_or(json!({}));
        let Some(command) = config["originalStatusLine"]["command"].as_str() else {
            return 0;
        };
        let Some(bash) = config["bash"].as_str() else {
            return 1;
        };
        let Ok(mut child) = Command::new(bash)
            .args(["-c", command])
            .creation_flags(NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        else {
            return 1;
        };
        let mut input = child.stdin.take().unwrap();
        let data = raw.to_vec();
        let writer = thread::spawn(move || {
            let _ = input.write_all(&data);
        });
        let output = child.stdout.take().unwrap();
        let errors = child.stderr.take().unwrap();
        let out = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = output.take(4 * 1024 * 1024).read_to_end(&mut bytes);
            bytes
        });
        let err = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = errors.take(4 * 1024 * 1024).read_to_end(&mut bytes);
            bytes
        });
        let start = Instant::now();
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code().unwrap_or(1),
                Err(_) => break 1,
                _ => {}
            }
            if start.elapsed() > Duration::from_secs(8) {
                let _ = child.kill();
                let _ = child.wait();
                break 1;
            }
            thread::sleep(Duration::from_millis(20));
        };
        // Дочерняя команда может оставить унаследованные pipes открытыми: не задерживаем hook сверх таймаута.
        if writer.is_finished() {
            let _ = writer.join();
        }
        if out.is_finished() {
            if let Ok(bytes) = out.join() {
                let _ = io::stdout().write_all(&bytes);
            }
        } else {
            for _ in 0..10 {
                if out.is_finished() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            if out.is_finished() {
                if let Ok(bytes) = out.join() {
                    let _ = io::stdout().write_all(&bytes);
                }
            }
        }
        if err.is_finished() {
            if let Ok(bytes) = err.join() {
                let _ = io::stderr().write_all(&bytes);
            }
        }
        code
    }
}
fn clean_entries(value: &Value) -> Vec<Value> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let mut entry = entry.clone();
            let hooks: Vec<_> = entry["hooks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|h| !h["command"].as_str().is_some_and(|c| c.contains(MARKER)))
                .cloned()
                .collect();
            if hooks.is_empty() {
                None
            } else {
                entry["hooks"] = json!(hooks);
                Some(entry)
            }
        })
        .collect()
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
pub fn find_bash() -> Option<PathBuf> {
    [
        std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from),
        Some(
            PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or("C:\\Program Files".into()))
                .join("Git/bin/bash.exe"),
        ),
        Some(
            PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
                .join("Programs/Git/bin/bash.exe"),
        ),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_file())
}
pub fn process_start(pid: u32) -> Option<i64> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let (mut created, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let ok = GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            None
        } else {
            Some(
                (((created.dwHighDateTime as u64) << 32 | created.dwLowDateTime as u64)
                    / 10_000_000) as i64
                    - 11_644_473_600,
            )
        }
    }
}
pub fn live(s: &Session) -> bool {
    if s.pid == 0 {
        now() - s.updated_at < 86400
    } else {
        process_start(s.pid)
            .is_some_and(|t| s.process_started_at == 0 || (t - s.process_started_at).abs() <= 2)
    }
}
fn find_claude() -> Option<(u32, i64)> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut rows = Vec::new();
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                rows.push((
                    entry.th32ProcessID,
                    entry.th32ParentProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..end]),
                ));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        let mut pid = std::process::id();
        for _ in 0..16 {
            let row = rows.iter().find(|r| r.0 == pid)?;
            if row.2.eq_ignore_ascii_case("claude.exe") {
                return Some((pid, process_start(pid)?));
            }
            pid = row.1;
        }
        None
    }
}
pub fn claude(root: &Path) -> Provider {
    let mut sessions = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("sessions")) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "json") {
                if let Some(s) = read_json::<Session>(&entry.path()) {
                    if live(&s) {
                        sessions.push(s)
                    }
                }
            }
        }
    }
    sort_sessions(&mut sessions);
    Provider {
        name: "Claude".into(),
        sessions,
        usage: read_json(&root.join("usage.json")),
        notice: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ctx() -> Context {
        let root = std::env::temp_dir().join(format!(
            "cwsb-native-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        Context {
            settings: root.join("settings.json"),
            root,
            exe: std::env::current_exe().unwrap(),
            test: true,
        }
    }
    #[test]
    fn install_roundtrip_preserves_foreign_hooks_and_statusline() {
        let c = ctx();
        let original = json!({"statusLine":{"type":"command","command":"printf 'исходная строка'","padding":2},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo own"}]}]},"other":true});
        write_json(&c.settings, &original).unwrap();
        c.install().unwrap();
        c.install().unwrap();
        let s: Value = read_json(&c.settings).unwrap();
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 2);
        let code = c.statusline(
            br#"{"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":2000000000}}}"#,
        );
        assert_eq!(code, 0);
        let u: Usage = read_json(&c.root.join("usage.json")).unwrap();
        assert_eq!(u.five_hour.unwrap().used, 42.);
        c.uninstall().unwrap();
        assert_eq!(read_json::<Value>(&c.settings).unwrap(), original);
        fs::remove_dir_all(c.root).unwrap();
    }
    #[test]
    fn hook_lifecycle_isolated_path() {
        let c = ctx();
        let p = json!({"session_id":"../../bad","cwd":"G:\\Проекты\\пример"});
        c.hook("SessionStart", &p).unwrap();
        c.hook("UserPromptSubmit", &p).unwrap();
        c.hook("PermissionRequest", &p).unwrap();
        let file = fs::read_dir(c.root.join("sessions"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let s: Session = read_json(&file).unwrap();
        assert_eq!(s.state, "permission");
        assert_eq!(s.project, "пример");
        assert!(s.started_at > 0);
        c.hook("Stop", &p).unwrap();
        assert_eq!(read_json::<Session>(&file).unwrap().started_at, 0);
        c.hook("SessionEnd", &p).unwrap();
        assert!(!file.exists());
        fs::remove_dir_all(c.root).unwrap();
    }
}
