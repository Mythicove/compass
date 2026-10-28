//! Tauri 命令层：薄薄一层，只做"取状态 → 调模块 → 落盘"。
//!
//! 命令一律是**具名**的，不提供 `request(url)` 这类通用转发：通用转发等于把桌面应用
//! 变成开放代理，也会让收紧后的 CSP 失去意义（见 api.rs 的模块注释）。

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Notify;

use crate::account::{self, Account, AccountView, now_unix};
use crate::api::{Project, SearchResponse, Version, png_data_url};
use crate::config::{self, Config};
use crate::error::Error;
use crate::install::{InstalledVersion, Installer};
use crate::instance::{self, Instance};
use crate::java::{self, JavaRuntime};
use crate::launch;
use crate::meta::{ManifestEntry, MetaClient};
use crate::net::Downloader;
use crate::paths::PathsInfo;
use crate::runner;
use crate::state::{AppState, GameExit, RunningGame};

/// 前端订阅的事件名：下载进度 / 游戏日志 / 游戏退出。
pub const PROGRESS_EVENT: &str = "compass://progress";
pub const LOG_EVENT: &str = "compass://game-log";
pub const EXIT_EVENT: &str = "compass://game-exit";

#[tauri::command]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> Result<Config, Error> {
    Ok(state.config.read().await.clone())
}

/// 保存全局设置。落盘成功后才更新内存态：写失败时 UI 不至于显示一个没生效的配置。
#[tauri::command]
pub async fn set_config(state: State<'_, Arc<AppState>>, config: Config) -> Result<Config, Error> {
    let sanitized = config.sanitize()?;
    config::save(&state.paths, &sanitized)?;
    *state.config.write().await = sanitized.clone();
    Ok(sanitized)
}

#[tauri::command]
pub async fn paths_info(state: State<'_, Arc<AppState>>) -> Result<PathsInfo, Error> {
    Ok(state.paths.info())
}

/// 启动时未能读取的内容（如损坏的配置文件）。读过一次即清空，避免每次进设置页都重复提示。
#[tauri::command]
pub async fn take_startup_warning(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<String>, Error> {
    Ok(state.startup_warning.write().await.take())
}

/// 当前登录态（本地存储，不发网络请求）；返回值**不含令牌**。
#[tauri::command]
pub async fn auth_status(state: State<'_, Arc<AppState>>) -> Result<Option<AccountView>, Error> {
    Ok(state.account.read().await.as_ref().map(Account::view))
}

/// 启动器登录：玩家名 + 该玩家的独立密码（ADR-0012）。
#[tauri::command]
pub async fn auth_login(
    state: State<'_, Arc<AppState>>,
    player_name: String,
    password: String,
) -> Result<AccountView, Error> {
    let api_url = state.config.read().await.api_url.clone();
    let resp = state
        .api
        .authenticate(&api_url, &player_name, &password)
        .await?;
    // 一条独立密码只证明一个玩家，selectedProfile 必然存在；缺失即后端契约破了
    let profile = resp
        .selected_profile
        .clone()
        .or_else(|| resp.available_profiles.first().cloned())
        .ok_or_else(|| Error::Invalid("authenticate response carries no profile".into()))?;
    let account = Account {
        player_name: profile.name,
        player_uuid: profile.id,
        access_token: resp.access_token,
        client_token: resp.client_token,
        api_url,
        signed_in_at: now_unix(),
    };
    account::save(&state.paths, &account)?;
    let view = account.view();
    *state.account.write().await = Some(account);
    Ok(view)
}

/// 登出：先尽力让服务端作废令牌（`/authserver/invalidate`，失败不阻塞本地登出），再清本地。
/// 不用 `/authserver/signout`——那个端点要玩家名 + 独立密码，而我们不存密码（也不该存）。
#[tauri::command]
pub async fn auth_logout(state: State<'_, Arc<AppState>>) -> Result<(), Error> {
    let current = state.account.read().await.clone();
    if let Some(a) = current {
        let _ = state
            .api
            .invalidate(&a.api_url, &a.access_token, &a.client_token)
            .await;
    }
    *state.account.write().await = None;
    account::clear(&state.paths)
}

/// 校验本地令牌是否仍然有效；无效则清掉登录态。
/// 网络错误**不**视为登出（离线启动不该把人踢出去），原样返回让前端提示。
#[tauri::command]
pub async fn auth_check(state: State<'_, Arc<AppState>>) -> Result<Option<AccountView>, Error> {
    let current = state.account.read().await.clone();
    let Some(a) = current else {
        return Ok(None);
    };
    match state
        .api
        .validate(&a.api_url, &a.access_token, &a.client_token)
        .await
    {
        Ok(()) => Ok(Some(a.view())),
        Err(Error::Http(_)) => Ok(Some(a.view())),
        Err(_) => {
            *state.account.write().await = None;
            account::clear(&state.paths)?;
            Ok(None)
        }
    }
}

/// 皮肤纹理，转成 `data:` URL 交给 skinview3d。
/// 走 Rust 而不是让 webview 直接 `<img src="https://mirror/skin/x">`：CSP 的 img-src 因此不必放行外站。
#[tauri::command]
pub async fn mirror_skin_data_url(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> Result<Option<String>, Error> {
    let base = state.config.read().await.api_url.clone();
    Ok(state
        .api
        .skin_png(&base, &name)
        .await?
        .map(|bytes| png_data_url(&bytes)))
}

#[tauri::command]
pub async fn hoard_search(
    state: State<'_, Arc<AppState>>,
    q: String,
    offset: i64,
    limit: i64,
) -> Result<SearchResponse<Project>, Error> {
    let base = state.config.read().await.hoard_url.clone();
    let limit = limit.clamp(1, 100);
    state
        .api
        .hoard_search(&base, &q, offset.max(0), limit)
        .await
}

#[tauri::command]
pub async fn hoard_versions(
    state: State<'_, Arc<AppState>>,
    project_id: String,
) -> Result<Vec<Version>, Error> {
    let base = state.config.read().await.hoard_url.clone();
    state.api.hoard_versions(&base, &project_id).await
}

// ---------------------------------------------------------------------------
// 版本 / 安装
// ---------------------------------------------------------------------------

/// 可安装的版本列表（来自 piston-meta 的清单）。快照默认不列：否则列表里全是噪音。
#[tauri::command]
pub async fn versions_list(
    state: State<'_, Arc<AppState>>,
    include_snapshots: bool,
) -> Result<Vec<ManifestEntry>, Error> {
    let cfg = state.config.read().await.clone();
    let meta = MetaClient::new(state.http.clone(), cfg.download.mirror.clone());
    let manifest = meta.manifest().await?;
    Ok(if include_snapshots {
        manifest.versions
    } else {
        manifest.releases().cloned().collect()
    })
}

/// 本地已装好的版本（合并后的版本 JSON 在磁盘上 = 可离线启动）。
#[tauri::command]
pub async fn versions_installed(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(state.paths.versions()) {
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().to_string();
            if entry.path().join(format!("{id}.json")).is_file() {
                out.push(id);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// 安装一个版本到全局缓存（幂等）。进度经 `compass://progress` 事件推送。
#[tauri::command]
pub async fn install_version(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    version_id: String,
) -> Result<InstalledVersion, Error> {
    let cfg = state.config.read().await.clone();
    let meta = MetaClient::new(state.http.clone(), cfg.download.mirror.clone());
    let dl = Downloader::new(
        state.http.clone(),
        cfg.download.concurrency,
        cfg.download.mirror.clone(),
    );
    let installer = Installer::new(meta, dl, state.paths.clone());

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let emitter = app.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            let _ = emitter.emit(PROGRESS_EVENT, p);
        }
    });
    let result = installer.install(&version_id, Some(tx)).await;
    // tx 随 result 之后被 drop，转发任务随之结束
    let _ = forwarder.await;
    result
}

// ---------------------------------------------------------------------------
// 实例
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn instance_list(state: State<'_, Arc<AppState>>) -> Result<Vec<Instance>, Error> {
    Ok(instance::list(&state.paths))
}

/// 建实例：目录 + instance.json + 游戏目录。版本本身由 `install_version` 单独装
/// （多个实例可以共享同一个版本，所以两件事不绑在一起）。
#[tauri::command]
pub async fn instance_create(
    state: State<'_, Arc<AppState>>,
    slug: String,
    name: String,
    version_id: String,
) -> Result<Instance, Error> {
    if instance::config_file(&state.paths, &slug)?.exists() {
        return Err(Error::Invalid(format!("instance already exists: {slug}")));
    }
    let cfg = state.config.read().await.clone();
    let inst = Instance {
        slug: slug.clone(),
        name: if name.trim().is_empty() {
            slug.clone()
        } else {
            name.trim().to_string()
        },
        version_id,
        created_at: now_unix(),
        memory_mb: Some(cfg.defaults.memory_mb),
        ..Instance::default()
    };
    instance::save(&state.paths, &inst)?;
    Ok(inst)
}

#[tauri::command]
pub async fn instance_delete(state: State<'_, Arc<AppState>>, slug: String) -> Result<(), Error> {
    // 正在跑的实例不能删：进程还握着 .minecraft 里的文件
    if state
        .running
        .read()
        .await
        .as_ref()
        .is_some_and(|r| r.instance == slug)
    {
        return Err(Error::Invalid(
            "stop the game before deleting its instance".into(),
        ));
    }
    instance::remove(&state.paths, &slug)
}

/// 这个实例的版本装好了吗（前端据此决定按钮是"安装"还是"启动"）。
#[tauri::command]
pub async fn instance_ready(state: State<'_, Arc<AppState>>, slug: String) -> Result<bool, Error> {
    let inst = instance::load(&state.paths, &slug)?;
    Ok(runner::installed_version_path(&state.paths, &inst.version_id).is_file())
}

/// 探测这个实例会用哪个 Java（UI 展示 + 让用户在启动前就知道缺 Java）。
#[tauri::command]
pub async fn instance_java(
    state: State<'_, Arc<AppState>>,
    slug: String,
) -> Result<Option<JavaRuntime>, Error> {
    let inst = instance::load(&state.paths, &slug)?;
    if let Some(p) = &inst.java_path {
        return Ok(java::probe(p).await);
    }
    let version = runner::read_installed_version(&state.paths, &inst.version_id)?;
    let (major, exact) = runner::java_requirement(&version);
    Ok(java::find(major, exact, Some(&state.paths.java())).await)
}

// ---------------------------------------------------------------------------
// 启动 / 停止
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchInfo {
    pub instance: String,
    pub version_id: String,
    pub java: PathBuf,
    pub java_version: u32,
}

/// 启动游戏。日志经 `compass://game-log` 推送，退出经 `compass://game-exit` 推送。
///
/// P1 只允许同时跑一个游戏进程：两个客户端用同一个账号令牌会让服务端的会话互相顶掉，
/// 而"多开"是 P2 的实例级选项（要先想清楚每个实例用哪个账号）。
#[tauri::command]
pub async fn instance_launch(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    slug: String,
) -> Result<LaunchInfo, Error> {
    if state.running.read().await.is_some() {
        return Err(Error::Invalid("a game is already running".into()));
    }
    let cfg = state.config.read().await.clone();
    let account = state
        .account
        .read()
        .await
        .clone()
        .ok_or(Error::NotSignedIn)?;
    let inst = instance::load(&state.paths, &slug)?;
    // 启动编排（读版本 / 选 Java / 备 injector / 拼命令行 / 解压 natives）只有一份实现，
    // 在 runner 里；冒烟测试走同一条路径，所以它测的就是用户会跑到的代码。
    let prepared = runner::prepare_launch(&state.paths, &state.http, &cfg, &inst, &account).await?;
    let plan = prepared.plan;

    let info = LaunchInfo {
        instance: slug.clone(),
        version_id: inst.version_id.clone(),
        java: plan.program.clone(),
        java_version: prepared.java_version,
    };
    // 先登记"正在运行"再拉起进程：反过来的话，进程可能在登记之前就退出了
    let kill = Arc::new(Notify::new());
    *state.running.write().await = Some(RunningGame {
        instance: slug.clone(),
        started_at: now_unix(),
        kill: Arc::clone(&kill),
    });

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let log_emitter = app.clone();
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            let _ = log_emitter.emit(LOG_EVENT, line);
        }
    });

    let supervisor_state = state.inner().clone();
    let supervisor_app = app.clone();
    tokio::spawn(async move {
        let result = launch::run(plan, tx, kill).await;
        *supervisor_state.running.write().await = None;
        let code = match result {
            Ok(code) => code,
            Err(e) => {
                let _ = supervisor_app.emit(
                    LOG_EVENT,
                    launch::LogLine {
                        stream: "stderr".to_string(),
                        line: format!("launcher: {e}"),
                    },
                );
                Some(-1)
            }
        };
        let _ = supervisor_app.emit(
            EXIT_EVENT,
            GameExit {
                instance: slug.clone(),
                code,
            },
        );
    });

    // 启动计数（失败也不该拦住启动本身）
    let mut updated = inst;
    updated.launches += 1;
    updated.last_launched_at = Some(now_unix());
    let _ = instance::save(&state.paths, &updated);

    Ok(info)
}

#[tauri::command]
pub async fn instance_stop(state: State<'_, Arc<AppState>>) -> Result<bool, Error> {
    let running = state.running.read().await;
    if let Some(game) = running.as_ref() {
        game.kill.notify_one();
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
pub async fn game_running(state: State<'_, Arc<AppState>>) -> Result<Option<String>, Error> {
    Ok(state
        .running
        .read()
        .await
        .as_ref()
        .map(|r| r.instance.clone()))
}
