use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct Limit {
    pub used: f64,
    pub resets_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Usage {
    pub five_hour: Option<Limit>,
    pub seven_day: Option<Limit>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Session {
    pub session_id: String,
    pub project: String,
    pub state: String,
    pub label: String,
    pub started_at: i64,
    pub updated_at: i64,
    pub pid: u32,
    pub process_started_at: i64,
    pub title: Option<String>,
}
impl Session {
    pub fn priority(&self) -> u8 {
        match self.state.as_str() {
            "permission" => 3,
            "tool" => 2,
            "thinking" => 1,
            _ => 0,
        }
    }
    pub fn task(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.project)
    }
    pub fn busy(&self) -> bool {
        matches!(self.state.as_str(), "thinking" | "tool")
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Provider {
    pub name: String,
    pub sessions: Vec<Session>,
    pub usage: Option<Usage>,
    pub notice: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase", default)]
pub struct Appearance {
    pub animation: String,
    pub color_mode: String,
    pub show_timer: bool,
    pub show_text: bool,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            animation: "Spark".into(),
            color_mode: "Orange".into(),
            show_timer: true,
            show_text: true,
        }
    }
}
impl Appearance {
    pub fn load(path: &Path) -> Self {
        let mut value: Self = read_json(path).unwrap_or_default();
        if !["Spark", "Unicode", "Clawd", "Orbit"].contains(&value.animation.as_str()) {
            value.animation = "Spark".into();
        }
        if !["Orange", "System"].contains(&value.color_mode.as_str()) {
            value.color_mode = "Orange".into();
        }
        value
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_json(path, self)
    }
}
pub fn now() -> i64 {
    Utc::now().timestamp()
}
pub fn data_dir() -> PathBuf {
    std::env::var_os("CWSB_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
                .join("ClaudeWindowsStatusBar")
        })
}
pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".codex"))
}
pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default())
}
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes)).ok()
}
pub fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    let result = fs::rename(&temp, path);
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub fn sort_sessions(sessions: &mut [Session]) {
    sessions.sort_by_key(|s| {
        (
            std::cmp::Reverse(s.priority()),
            std::cmp::Reverse(s.updated_at),
        )
    });
}
pub fn duration(seconds: i64) -> String {
    let s = seconds.max(0);
    if s >= 86400 {
        format!("{} д {} ч", s / 86400, s % 86400 / 3600)
    } else if s >= 3600 {
        format!("{} ч {} мин", s / 3600, s % 3600 / 60)
    } else {
        format!("{} мин {} с", s / 60, s % 60)
    }
}
pub fn task_name(text: &str) -> Option<String> {
    let s = text.trim();
    if s.starts_with("# AGENTS.md")
        || [
            "<environment_context>",
            "<INSTRUCTIONS>",
            "<permissions",
            "<turn_aborted>",
            "<system",
        ]
        .iter()
        .any(|p| s.starts_with(p))
    {
        return None;
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() < 8 {
        None
    } else {
        Some(s.chars().take(2000).collect())
    }
}
pub fn project(cwd: &str) -> String {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("Codex")
        .into()
}
pub fn demo() -> Vec<Provider> {
    let n = now();
    [
        ("Claude", 70., 30., "Обдумывает ответ"),
        ("Codex", 19., 88., "Редактирует файлы"),
    ]
    .into_iter()
    .map(|(name, five, seven, label)| Provider {
        name: name.into(),
        sessions: vec![Session {
            session_id: format!("demo-{name}"),
            project: "ClaudeWindowsStatusBar".into(),
            state: "thinking".into(),
            label: label.into(),
            started_at: n - 135,
            updated_at: n,
            title: (name == "Codex")
                .then(|| "Добавить настройки внешнего вида и название задачи Codex".into()),
            ..Default::default()
        }],
        usage: Some(Usage {
            five_hour: Some(Limit {
                used: five,
                resets_at: n + 5040,
            }),
            seven_day: Some(Limit {
                used: seven,
                resets_at: n + 367200,
            }),
            updated_at: Utc::now(),
        }),
        notice: Some("Демонстрационные данные".into()),
    })
    .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_names_ignore_instructions() {
        assert!(task_name("# AGENTS.md instructions").is_none());
        assert_eq!(
            task_name("  Сделать  задачу  ").as_deref(),
            Some("Сделать задачу")
        );
    }
    #[test]
    fn duration_and_priority() {
        assert_eq!(duration(5040), "1 ч 24 мин");
        assert_eq!(duration(-1), "0 мин 0 с");
        assert!(
            Session {
                state: "permission".into(),
                ..Default::default()
            }
            .priority()
                > 2
        );
    }
    #[test]
    fn appearance_compatibility() {
        let a: Appearance = serde_json::from_str(
            r#"{"Animation":"Clawd","ColorMode":"System","ShowTimer":false,"ShowText":false}"#,
        )
        .unwrap();
        assert!(!a.show_timer);
        assert_eq!(a.animation, "Clawd");
    }
}
