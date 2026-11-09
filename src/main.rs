#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod integration;
mod model;
mod monitor;
mod ui;
use std::io::{self, Read};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let context = match integration::Context::current() {
        Ok(c) => c,
        Err(_) => return,
    };
    match args.get(1).map(String::as_str) {
        Some("--install") => {
            if let Err(e) = context.install() {
                ui::error(&e);
                std::process::exit(1)
            }
        }
        Some("--uninstall") => {
            if let Err(e) = context.uninstall() {
                ui::error(&e);
                std::process::exit(1)
            }
        }
        Some("--hook" | "--statusline") => {
            let mut raw = Vec::new();
            let _ = io::stdin().take(16 * 1024 * 1024).read_to_end(&mut raw);
            if args[1] == "--statusline" {
                std::process::exit(context.statusline(&raw))
            }
            if let (Some(event), Ok(payload)) = (args.get(2), serde_json::from_slice(&raw)) {
                let _ = context.hook(event, &payload);
            }
        }
        Some("--diagnostics") => {
            let mut monitor = monitor::Monitor::new(model::codex_home());
            let codex = monitor.poll();
            if let Some(path) = args.get(2) {
                let safe = serde_json::json!({"engine":"Rust","sessionCount":codex.sessions.len(),"titledSessions":codex.sessions.iter().filter(|s|s.title.is_some()).count(),"usage":codex.usage});
                let _ = model::write_json(std::path::Path::new(path), &safe);
            }
        }
        _ => {
            if let Err(e) = ui::run(
                args.iter().any(|a| a == "--demo"),
                args.iter()
                    .position(|a| a == "--screenshot")
                    .and_then(|i| args.get(i + 1))
                    .cloned(),
            ) {
                ui::error(&e);
            }
        }
    }
}
