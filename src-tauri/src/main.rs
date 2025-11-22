// src-tauri/src/main.rs
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // "mineger_lib" is usually the name of the package + "_lib" in some templates, 
    // OR just the package name if you are importing it.
    // However, since we are inside the same crate structure, we just call the function.
    
    mineger_lib::run(); 
}