use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{Foundation::ERROR_SUCCESS, System::Registry::*};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct InstalledAgents {
    pub claude: bool,
    pub codex: bool,
}
impl InstalledAgents {
    pub fn brand_mode(self) -> &'static str {
        match (self.claude, self.codex) {
            (true, true) => "Split",
            (false, true) => "Codex",
            _ => "Orange",
        }
    }
}
pub fn detect() -> InstalledAgents {
    let directories = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .unwrap_or_default();
    let home = crate::model::home();
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default());
    let roaming = PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default());
    let program_files = PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or_default());
    let mut installed = detect_paths(&directories, &home, &local, &roaming, &program_files);
    installed.claude |= app_path("claude.exe");
    installed.codex |= app_path("codex.exe");
    installed
}
fn detect_paths(
    directories: &[PathBuf],
    home: &Path,
    local: &Path,
    roaming: &Path,
    program_files: &Path,
) -> InstalledAgents {
    let in_path = |name: &str| {
        directories.iter().any(|dir| {
            ["exe", "cmd", "bat", "ps1"]
                .iter()
                .any(|ext| dir.join(format!("{name}.{ext}")).is_file())
        })
    };
    let standard = |name: &str| {
        let mut paths = vec![
            home.join(".local/bin").join(format!("{name}.exe")),
            home.join(".cargo/bin").join(format!("{name}.exe")),
            roaming.join("npm").join(format!("{name}.cmd")),
            local
                .join("Microsoft/WindowsApps")
                .join(format!("{name}.exe")),
        ];
        if name == "claude" {
            paths.extend([
                local.join("Programs/Claude/Claude.exe"),
                local.join("AnthropicClaude/Claude.exe"),
                program_files.join("Claude/Claude.exe"),
                program_files.join("AnthropicClaude/Claude.exe"),
            ]);
            for root in [local.join("AnthropicClaude"), local.join("Programs/Claude")] {
                if let Ok(entries) = fs::read_dir(root) {
                    for entry in entries.flatten() {
                        if entry.file_name().to_string_lossy().starts_with("app-") {
                            paths.push(entry.path().join("Claude.exe"));
                        }
                    }
                }
            }
        } else {
            paths.extend([
                local.join("Programs/OpenAI/Codex/bin/codex.exe"),
                local.join("OpenAI/Codex/bin/codex.exe"),
                local.join("Programs/Codex/Codex.exe"),
                program_files.join("OpenAI/Codex/Codex.exe"),
                program_files.join("Codex/Codex.exe"),
            ]);
        }
        paths.iter().any(|p| p.is_file())
    };
    InstalledAgents {
        claude: in_path("claude") || standard("claude"),
        codex: in_path("codex") || standard("codex"),
    }
}
fn app_path(name: &str) -> bool {
    unsafe {
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let key = wide(&format!(
            r"Software\Microsoft\Windows\CurrentVersion\App Paths\{name}"
        ));
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            let mut size = 0;
            if RegGetValueW(
                hive,
                key.as_ptr(),
                null(),
                RRF_RT_REG_SZ,
                null_mut(),
                null_mut(),
                &mut size,
            ) != ERROR_SUCCESS
                || size > 65536
            {
                continue;
            }
            let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
            if RegGetValueW(
                hive,
                key.as_ptr(),
                null(),
                RRF_RT_REG_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            ) == ERROR_SUCCESS
            {
                let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
                let value = String::from_utf16_lossy(&buffer[..end]);
                if Path::new(value.trim().trim_matches('"')).is_file() {
                    return true;
                }
            }
        }
        false
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palette_uses_installation_not_session_state() {
        assert_eq!(
            InstalledAgents {
                claude: true,
                codex: false
            }
            .brand_mode(),
            "Orange"
        );
        assert_eq!(
            InstalledAgents {
                claude: true,
                codex: true
            }
            .brand_mode(),
            "Split"
        );
        assert_eq!(
            InstalledAgents {
                claude: false,
                codex: true
            }
            .brand_mode(),
            "Codex"
        );
    }
    #[test]
    fn settings_directories_are_not_installations() {
        let root = std::env::temp_dir().join(format!(
            "cwsb-installed-{}-{}",
            std::process::id(),
            crate::model::now()
        ));
        fs::create_dir_all(root.join("home/.claude")).unwrap();
        fs::create_dir_all(root.join("home/.codex/sessions")).unwrap();
        let probe = || {
            detect_paths(
                &[root.join("path")],
                &root.join("home"),
                &root.join("local"),
                &root.join("roaming"),
                &root.join("programs"),
            )
        };
        assert_eq!(probe(), InstalledAgents::default());
        fs::create_dir_all(root.join("home/.local/bin")).unwrap();
        fs::write(root.join("home/.local/bin/claude.exe"), b"fixture").unwrap();
        assert_eq!(probe().brand_mode(), "Orange");
        fs::create_dir_all(root.join("local/Programs/OpenAI/Codex/bin")).unwrap();
        fs::write(
            root.join("local/Programs/OpenAI/Codex/bin/codex.exe"),
            b"fixture",
        )
        .unwrap();
        assert_eq!(probe().brand_mode(), "Split");
        fs::remove_file(root.join("home/.local/bin/claude.exe")).unwrap();
        assert_eq!(probe().brand_mode(), "Codex");
        fs::remove_dir_all(root).unwrap();
    }
}
