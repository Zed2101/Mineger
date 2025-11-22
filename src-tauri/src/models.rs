// src-tauri/src/models.rs
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModEntry {
    pub name: String,
    pub hash: String,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServerDataFile {
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: String,
    pub last_played: String,
    
    #[serde(default)] 
    pub mods: Vec<ModEntry>,

    #[serde(default)]
    pub last_scan_timestamp: u64, 
}

#[derive(Serialize, Debug)]
pub struct ServerEntry {
    pub id: String,       
    pub uuid: String,
    pub name: String,
    pub version: String,
    pub icon: String,
    pub status: String,   
    pub last_played: String,
    pub mods: Vec<ModEntry>,
    pub properties: HashMap<String, String>,
    pub mods_count: usize, 
}