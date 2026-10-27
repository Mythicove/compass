//! 全局状态。用 `Arc` 包起来交给 Tauri 的 `manage`，命令按 `State<'_, Arc<AppState>>` 取用。

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::account::Account;
use crate::api::ApiClient;
use crate::config::Config;
use crate::paths::Paths;

pub struct AppState {
    pub paths: Paths,
    pub api: ApiClient,
    pub config: RwLock<Config>,
    pub account: RwLock<Option<Account>>,
    /// 启动时没能读到的东西（如损坏的 compass.json）记在这里，由设置页显示一次：
    /// 桌面应用没有控制台，静默回落到默认值会让用户以为设置"莫名消失"。
    pub startup_warning: RwLock<Option<String>>,
}

impl AppState {
    /// `startup_warning` 由构造方给出（setup 里是同步上下文，不方便再去抢异步锁）。
    pub fn new(
        paths: Paths,
        api: ApiClient,
        config: Config,
        account: Option<Account>,
        startup_warning: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            paths,
            api,
            config: RwLock::new(config),
            account: RwLock::new(account),
            startup_warning: RwLock::new(startup_warning),
        })
    }
}
