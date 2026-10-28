//! Mythic Compass 的 Rust 核心。
//!
//! 分层（计划文档 P0 决策 2）：重逻辑全在这里——路径布局、配置、账号、后端 HTTP，
//! 以及后续几个阶段的版本解析、下载调度、启动参数构造与进程管理。
//! 前端只做视图，通过**具名命令**取数据；不提供"转发任意 URL"的命令。

pub mod account;
pub mod api;
pub mod assets;
pub mod authlibinjector;
pub mod commands;
pub mod config;
pub mod content;
pub mod error;
pub mod install;
pub mod instance;
pub mod java;
pub mod launch;
pub mod meta;
pub mod net;
pub mod paths;
pub mod runner;
pub mod state;
pub mod version;

use tauri::Manager;

use crate::api::ApiClient;
use crate::paths::Paths;
use crate::state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // 原生文件选择器：导入 mod/资源包、指定 Java 路径都靠它。
        // 不用 HTML `<input type=file>`——webview 拿到的是不透明句柄，
        // 而复制文件需要真实的文件系统路径。
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let paths = Paths::new(app.path().app_data_dir()?);
            paths.ensure().map_err(|e| e.to_string())?;
            let (account, account_warning) = account::load(&paths);

            // 配置损坏不阻止启动：用默认值起来，并把原因交给设置页显示一次。
            // 直接崩不该由一个坏 JSON 造成，静默回落又会让用户以为设置"莫名消失"。
            let (config, warning) = match config::load(&paths) {
                Ok(c) => (c, None),
                Err(e) => (
                    config::Config::default(),
                    Some(format!(
                        "{} 无法读取，已回落到默认设置：{e}",
                        paths.config_file().display()
                    )),
                ),
            };
            // accounts.json 损坏的警告走同一机制（文件本身保留，见 account::load）
            let warning = match (warning, account_warning) {
                (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
                (a, b) => a.or(b),
            };

            let http = api::http_client().map_err(|e| e.to_string())?;
            let api = ApiClient::new(http.clone());
            app.manage(AppState::new(paths, api, http, config, account, warning));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_config,
            commands::paths_info,
            commands::take_startup_warning,
            commands::auth_login,
            commands::auth_logout,
            commands::auth_check,
            commands::mirror_skin_data_url,
            commands::hoard_search,
            commands::hoard_versions,
            commands::versions_list,
            commands::versions_installed,
            commands::install_version,
            commands::instance_list,
            commands::instance_create,
            commands::instance_delete,
            commands::instance_update,
            commands::instance_duplicate,
            commands::instance_java,
            commands::content_list,
            commands::content_set_enabled,
            commands::content_import,
            commands::content_remove,
            commands::instance_launch,
            commands::instance_stop,
            commands::game_running
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
