use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{OnceLock, RwLock},
};
use windows_sys::Win32::Globalization::{GetUserDefaultUILanguage, LCIDToLocaleName};

#[derive(Clone, Default, Deserialize)]
struct Metadata {
    code: String,
    name: String,
}
#[derive(Clone, Default, Deserialize)]
struct LanguageFile {
    #[serde(rename = "_meta", default)]
    meta: Metadata,
    #[serde(default)]
    strings: HashMap<String, String>,
}
#[derive(Clone)]
struct Catalog {
    languages: BTreeMap<String, LanguageFile>,
    source: HashMap<String, String>,
    patterns: Vec<(String, String)>,
    selected: String,
}
impl Catalog {
    fn builtins() -> Self {
        let ru: LanguageFile = serde_json::from_str(include_str!("../locales/ru.json"))
            .expect("valid built-in Russian locale");
        let en: LanguageFile = serde_json::from_str(include_str!("../locales/en.json"))
            .expect("valid built-in English locale");
        let mut source = HashMap::new();
        let mut patterns = Vec::new();
        for (key, value) in &ru.strings {
            if value.contains('{') {
                patterns.push((key.clone(), value.clone()));
            } else {
                source.insert(value.clone(), key.clone());
            }
        }
        patterns.sort_by_key(|(_, pattern)| {
            std::cmp::Reverse(pattern.chars().filter(|c| *c != '{' && *c != '}').count())
        });
        Self {
            languages: BTreeMap::from([("ru".into(), ru), ("en".into(), en)]),
            source,
            patterns,
            selected: "en".into(),
        }
    }
    fn load_directory(&mut self, directory: &Path) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        for path in paths.into_iter().take(64) {
            if !fs::metadata(&path).is_ok_and(|m| m.len() <= 1024 * 1024) {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
            let Ok(mut file) = serde_json::from_slice::<LanguageFile>(bytes) else {
                continue;
            };
            let code = normalize(&file.meta.code);
            if !valid_code(&code)
                || file.meta.name.trim().is_empty()
                || path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(normalize)
                    .as_deref()
                    != Some(code.as_str())
            {
                continue;
            }
            file.meta.code = code.clone();
            if let Some(existing) = self.languages.get_mut(&code) {
                existing.strings.extend(file.strings);
                existing.meta.name = file.meta.name;
            } else {
                self.languages.insert(code, file);
            }
        }
    }
    fn select(&mut self, preference: &str, system: &str) {
        let code = normalize(if preference.eq_ignore_ascii_case("auto") {
            system
        } else {
            preference
        });
        self.selected = if self.languages.contains_key(&code) {
            code
        } else {
            let base = code.split('-').next().unwrap_or("en");
            if self.languages.contains_key(base) {
                base.into()
            } else {
                "en".into()
            }
        };
    }
    fn value(&self, key: &str) -> Option<&str> {
        self.languages
            .get(&self.selected)
            .and_then(|l| l.strings.get(key))
            .or_else(|| self.languages.get("en").and_then(|l| l.strings.get(key)))
            .map(String::as_str)
    }
    fn translate(&self, source: &str, depth: u8) -> String {
        if depth > 4 {
            return source.into();
        }
        if let Some(key) = self.source.get(source) {
            return self.value(key).unwrap_or(source).into();
        }
        for (key, pattern) in &self.patterns {
            if let Some(values) = capture(pattern, source) {
                let mut output = self.value(key).unwrap_or(pattern).to_owned();
                for (name, value) in values {
                    let value = if matches!(name.as_str(), "window" | "name" | "duration") {
                        self.translate(&value, depth + 1)
                    } else {
                        value
                    };
                    output = output.replace(&format!("{{{name}}}"), &value);
                }
                return output;
            }
        }
        if source.contains(" · ") {
            return source
                .split(" · ")
                .map(|part| self.translate(part, depth + 1))
                .collect::<Vec<_>>()
                .join(" · ");
        }
        source.into()
    }
}
fn normalize(code: &str) -> String {
    code.trim().replace('_', "-").to_ascii_lowercase()
}
fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 32
        && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}
fn capture(pattern: &str, source: &str) -> Option<Vec<(String, String)>> {
    let (mut pattern, mut source) = (pattern, source);
    let mut values = Vec::new();
    loop {
        let Some(open) = pattern.find('{') else {
            return (pattern == source).then_some(values);
        };
        let literal = &pattern[..open];
        source = source.strip_prefix(literal)?;
        pattern = &pattern[open + 1..];
        let close = pattern.find('}')?;
        let name = &pattern[..close];
        pattern = &pattern[close + 1..];
        let next = pattern.find('{').unwrap_or(pattern.len());
        let delimiter = &pattern[..next];
        if delimiter.is_empty() {
            if pattern.is_empty() {
                values.push((name.into(), source.into()));
                return Some(values);
            }
            return None;
        }
        let end = source.find(delimiter)?;
        values.push((name.into(), source[..end].into()));
        source = &source[end..];
    }
}
static CATALOG: OnceLock<RwLock<Catalog>> = OnceLock::new();
fn catalog() -> &'static RwLock<Catalog> {
    CATALOG.get_or_init(|| RwLock::new(Catalog::builtins()))
}
pub fn directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("locales")))
        .unwrap_or_else(|| PathBuf::from("locales"))
}
pub fn system_language() -> String {
    unsafe {
        let id = GetUserDefaultUILanguage();
        let mut buffer = [0u16; 85];
        let count = LCIDToLocaleName(id as u32, buffer.as_mut_ptr(), buffer.len() as i32, 0);
        if count > 1 {
            String::from_utf16_lossy(&buffer[..count as usize - 1])
        } else {
            "en".into()
        }
    }
}
pub fn init(preference: &str) {
    let mut value = Catalog::builtins();
    value.load_directory(&directory());
    value.select(preference, &system_language());
    *catalog().write().unwrap_or_else(|e| e.into_inner()) = value;
}
pub fn tr(source: &str) -> String {
    catalog()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .translate(source, 0)
}
pub fn languages() -> Vec<(String, String)> {
    catalog()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .languages
        .iter()
        .map(|(code, file)| (code.clone(), file.meta.name.clone()))
        .collect()
}
pub fn current() -> String {
    catalog()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .selected
        .clone()
}
pub fn duration(seconds: i64) -> String {
    tr(&crate::model::duration(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_language_and_english_fallback() {
        let mut c = Catalog::builtins();
        c.select("auto", "en-US");
        assert_eq!(c.translate("Выход", 0), "Exit");
        c.select("auto", "ru-RU");
        assert_eq!(c.translate("Выход", 0), "Выход");
        c.select("auto", "de-DE");
        assert_eq!(c.selected, "en");
    }
    #[test]
    fn templates_preserve_values_and_translate_duration() {
        let c = Catalog::builtins();
        assert_eq!(c.translate("Сброс через 1 ч 24 мин", 0), "Resets in 1h 24m");
        assert_eq!(
            c.translate("Codex · Лимит 5 часов", 0),
            "Codex · Limit: 5 hours"
        );
        assert_eq!(
            c.translate("Использовано 90% лимита. Текущее значение: 91%.", 0),
            "90% of the limit used. Current usage: 91%."
        );
    }
    #[test]
    fn catalogs_have_the_same_keys_and_valid_templates() {
        let c = Catalog::builtins();
        let ru = &c.languages["ru"].strings;
        let en = &c.languages["en"].strings;
        assert_eq!(ru.len(), en.len());
        for key in ru.keys() {
            assert!(en.contains_key(key), "{key}");
        }
        assert!(capture("Обновлено: {time}", "Обновлено: 15:30").is_some());
    }
    #[test]
    fn extra_language_is_loaded_and_missing_strings_fall_back_to_english() {
        let root = std::env::temp_dir().join(format!(
            "cwsb-locale-{}-{}",
            std::process::id(),
            crate::model::now()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("de.json"),
            r#"{"_meta":{"code":"de","name":"Deutsch"},"strings":{"menu.exit":"Beenden"}}"#,
        )
        .unwrap();
        let mut c = Catalog::builtins();
        c.load_directory(&root);
        c.select("de", "ru-RU");
        assert_eq!(c.translate("Выход", 0), "Beenden");
        assert_eq!(c.translate("Показать состояние", 0), "Show status");
        fs::remove_dir_all(root).unwrap();
    }
}
