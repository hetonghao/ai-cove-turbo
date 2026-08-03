//! AI Cove Turbo desktop entry point.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> tauri::Result<()> {
    ai_cove_turbo::run()
}
