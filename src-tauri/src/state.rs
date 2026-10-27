//! 全局状态。用 `Arc` 包起来交给 Tauri 的 `manage`，命令按 `State<'_, Arc<AppState>>` 取用。

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{Notify, RwLock};

use crate::account::Account;
use crate::api::ApiClient;
use crate::config::Config;
use crate::paths::Paths;

/// 正在运行的游戏进程。`kill` 用于用户主动停止——它与"游戏自己退出"必须能区分，
/// 否则崩溃诊断（P5）会把每次手动停止都当成一次崩溃。
pub struct RunningGame {
    pub instance: String,
    pub started_at: u64,
    pub kill: Arc<Notify>,
}

/// 游戏退出事件（`compass://game-exit`）。`code` 为 None 表示被我们主动停止。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameExit {
    pub instance: String,
    pub code: Option<i32>,
}

pub struct AppState {
    pub paths: Paths,
    pub api: ApiClient,
    /// 下载与元数据共用的 HTTP 客户端（连接池复用）。
    pub http: reqwest::Client,
    pub config: RwLock<Config>,
    pub account: RwLock<Option<Account>>,
    pub running: RwLock<Option<RunningGame>>,
    /// 启动时没能读到的东西（如损坏的 compass.json）记在这里，由设置页显示一次：
    /// 桌面应用没有控制台，静默回落到默认值会让用户以为设置"莫名消失"。
    pub startup_warning: RwLock<Option<String>>,
}

impl AppState {
    /// `startup_warning` 由构造方给出（setup 里是同步上下文，不方便再去抢异步锁）。
    pub fn new(
        paths: Paths,
        api: ApiClient,
        http: reqwest::Client,
        config: Config,
        account: Option<Account>,
        startup_warning: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            paths,
            api,
            http,
            config: RwLock::new(config),
            account: RwLock::new(account),
            running: RwLock::new(None),
            startup_warning: RwLock::new(startup_warning),
        })
    }
}
