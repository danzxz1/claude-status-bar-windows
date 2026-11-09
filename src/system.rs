use crate::model::Session;
use std::{
    collections::HashSet,
    mem::{size_of, zeroed},
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Diagnostics::ToolHelp::*, Registry::*, Threading::*},
    UI::WindowsAndMessaging::*,
};
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "ClaudeWindowsStatusBar";
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn autostart_enabled() -> bool {
    unsafe {
        let mut size = 0;
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(RUN_NAME).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        ) == ERROR_SUCCESS
            && size > 2
    }
}
pub fn set_autostart(enabled: bool, exe: &Path) -> Result<(), String> {
    unsafe {
        let mut key = null_mut();
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            null(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        );
        if status != ERROR_SUCCESS {
            return Err(format!(
                "Не удалось открыть настройки автозапуска: {status}"
            ));
        }
        let result = if enabled {
            let command = wide(&format!("\"{}\"", exe.display()));
            RegSetValueExW(
                key,
                wide(RUN_NAME).as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key, wide(RUN_NAME).as_ptr())
        };
        RegCloseKey(key);
        if result == ERROR_SUCCESS || (!enabled && result == ERROR_FILE_NOT_FOUND) {
            Ok(())
        } else {
            Err(format!("Не удалось изменить автозапуск: {result}"))
        }
    }
}
#[derive(Clone, Debug)]
pub struct WindowTarget {
    pub hwnd: usize,
    pub title: String,
    pub process: String,
    pub pid: u32,
    pub score: u32,
}
unsafe extern "system" fn enumerate(hwnd: HWND, data: LPARAM) -> i32 {
    unsafe {
        if IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
            return 1;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return 1;
        }
        let mut title = vec![0; len as usize + 1];
        GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == std::process::id() {
            return 1;
        }
        let process = process_name(pid);
        (&mut *(data as *mut Vec<WindowTarget>)).push(WindowTarget {
            hwnd: hwnd as usize,
            title: String::from_utf16_lossy(&title[..len as usize]),
            process,
            pid,
            score: 0,
        });
        1
    }
}
unsafe fn process_name(pid: u32) -> String {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return String::new();
        }
        let mut buffer = vec![0u16; 32768];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buffer[..size as usize])
                .rsplit('\\')
                .next()
                .unwrap_or("")
                .to_lowercase()
        }
    }
}
fn ancestors(pid: u32) -> HashSet<u32> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return HashSet::new();
        }
        let mut entries = Vec::new();
        let mut e: PROCESSENTRY32W = zeroed();
        e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut e) != 0 {
            loop {
                entries.push((e.th32ProcessID, e.th32ParentProcessID));
                if Process32NextW(snapshot, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        let mut ids = HashSet::new();
        let mut current = pid;
        for _ in 0..12 {
            if !ids.insert(current) {
                break;
            }
            let Some((_, parent)) = entries.iter().find(|(id, _)| *id == current) else {
                break;
            };
            current = *parent;
        }
        ids
    }
}
pub fn candidates(provider: &str, session: &Session) -> Vec<WindowTarget> {
    unsafe {
        let mut windows: Vec<WindowTarget> = Vec::new();
        EnumWindows(
            Some(enumerate),
            (&mut windows as *mut Vec<WindowTarget>) as isize,
        );
        let ancestors = if session.pid > 0 {
            ancestors(session.pid)
        } else {
            HashSet::new()
        };
        for window in &mut windows {
            window.score = target_score(provider, session, window, &ancestors);
        }
        windows.sort_by_key(|w| (std::cmp::Reverse(w.score), w.title.clone()));
        windows
    }
}
fn target_score(
    provider: &str,
    session: &Session,
    window: &WindowTarget,
    ancestors: &HashSet<u32>,
) -> u32 {
    let mut score = 0;
    let title = window.title.to_lowercase();
    if ancestors.contains(&window.pid) && window.process != "explorer.exe" {
        score += 100;
    }
    if session
        .title
        .as_ref()
        .is_some_and(|t| t.chars().count() >= 8 && title.contains(&t.to_lowercase()))
    {
        score += 80;
    }
    let project = session.project.to_lowercase();
    if project.chars().count() >= 4
        && !["localuser", "users", "desktop", "documents"].contains(&project.as_str())
        && title.contains(&project)
    {
        score += 30;
    }
    if provider == "Codex" && window.process.contains("codex") {
        score += 20;
    }
    if provider == "Claude" && window.process == "claude.exe" {
        score += 20;
    }
    if window.process.contains("windowsterminal")
        || window.process == "code.exe"
        || window.process == "wezterm-gui.exe"
    {
        score += 1;
    }
    score
}
pub fn activate(hwnd: usize) -> bool {
    unsafe {
        let hwnd = hwnd as HWND;
        if IsWindow(hwnd) == 0 {
            return false;
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd) != 0
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_title_beats_generic_terminal() {
        let session = Session {
            title: Some("Исправить отчёт в портале".into()),
            project: "localuser".into(),
            ..Default::default()
        };
        let window = WindowTarget {
            hwnd: 1,
            title: "Исправить отчёт в портале — Codex".into(),
            process: "codex.exe".into(),
            pid: 2,
            score: 0,
        };
        assert!(target_score("Codex", &session, &window, &HashSet::new()) >= 100);
        let terminal = WindowTarget {
            title: "Windows PowerShell".into(),
            process: "windowsterminal.exe".into(),
            ..window
        };
        assert_eq!(
            target_score("Codex", &session, &terminal, &HashSet::new()),
            1
        );
    }
}
