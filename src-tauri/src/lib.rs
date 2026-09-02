// src-tauri/src/lib.rs

pub mod backup;
pub mod commands;
pub mod create;
pub mod events;
pub mod host;
pub mod i18n;
pub mod icons;
pub mod java;
pub mod launch;
pub mod loaders;
pub mod modsvc;
pub mod metrics;
pub mod models;
pub mod packs;
pub mod paths;
pub mod providers;
pub mod process;
pub mod service;
pub mod servericon;
pub mod settings;
pub mod upnp;
pub mod utils;

use std::time::Duration;
use tauri::RunEvent;

/// Tempo massimo concesso ai server per salvare e chiudersi quando l'app viene chiusa.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Scalda la cache delle Java in background: il primo avvio sarà immediato.
            std::thread::spawn(|| {
                java::detect_runtimes();
            });

            // Host remoto, se abilitato nelle impostazioni
            let handle = app.handle().clone();
            let s = settings::load(&handle);

            // Lingua dei messaggi del backend: preferenza salvata o, se assente,
            // quella del sistema operativo (con riserva sull'inglese).
            i18n::set_language(&i18n::resolve(&s.language));

            if let Err(e) = host::apply(&handle, &s) {
                println!("[Mineger] Listener HTTP non avviato: {}", e);
            }

            // Controllo aggiornamenti dei modpack installati da link (dopo 20 s, poi ogni 6 h)
            packs::start_periodic_checks(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_servers,
            commands::start_server,
            commands::stop_server,
            commands::kill_server,
            commands::send_command,
            commands::get_recent_logs,
            commands::update_server_info,
            commands::delete_server,
            commands::get_server_disk_usage,
            commands::update_launch_config,
            commands::save_server_properties,
            commands::toggle_mod,
            commands::delete_mod,
            commands::add_mods,
            commands::get_server_metrics,
            commands::list_backups,
            commands::create_backup,
            commands::accept_eula_cmd,
            commands::get_vanilla_versions,
            commands::create_vanilla_server,
            commands::get_mc_versions,
            commands::get_loader_versions,
            commands::create_server,
            commands::get_mod_context,
            commands::search_mods,
            commands::get_mod_versions,
            commands::install_mod,
            commands::check_mod_updates,
            commands::update_mod,
            commands::import_server_zip,
            commands::open_server_folder,
            commands::open_app_folder,
            commands::open_url,
            commands::get_java_runtimes,
            commands::get_app_info,
            commands::get_settings,
            commands::get_host_status,
            commands::set_host_config,
            commands::regenerate_host_token,
            commands::add_remote_host,
            commands::remove_remote_host,
            commands::set_server_order,
            commands::list_icons,
            commands::import_icon_from_path,
            commands::pick_icon_file,
            commands::delete_icon,
            commands::list_webhooks,
            commands::create_webhook,
            commands::set_webhook_enabled,
            commands::delete_webhook,
            commands::get_webhook_calls,
            commands::test_webhook,
            commands::get_server_icon,
            commands::set_server_icon_from_bytes,
            commands::set_server_icon_from_path,
            commands::pick_server_icon_file,
            commands::remove_server_icon,
            commands::resolve_pack_link,
            commands::install_pack,
            commands::check_updates,
            commands::get_cached_updates,
            commands::update_pack_server,
            commands::set_curseforge_key,
            commands::curseforge_configured,
            commands::get_language,
            commands::get_system_language,
            commands::set_language,
            commands::list_languages,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // Contatori webhook non ancora scritti, host remoto (chiude anche la
                // porta UPnP), poi i server Java.
                host::flush_stats(app);
                host::shutdown_blocking();
                process::shutdown_all(SHUTDOWN_TIMEOUT);
            }
        });
}
