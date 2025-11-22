// src-tauri/src/lib.rs

// Declare the modules so Rust knows they exist
pub mod models;
pub mod utils;
pub mod commands;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Register the commands from the commands module
        .invoke_handler(tauri::generate_handler![
            commands::get_servers
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}