//! Tauri 命令层：薄薄一层，只做"取状态 → 调模块 → 落盘"。
//!
//! 命令一律是**具名**的，不提供 `request(url)` 这类通用转发：通用转发等于把桌面应用
//! 变成开放代理，也会让收紧后的 CSP 失去意义（见 api.rs 的模块注释）。

use std::sync::Arc;

use tauri::State;

use crate::account::{self, Account, AccountView, now_unix};
use crate::api::png_data_url;
use crate::api::{Project, SearchResponse, Version};
use crate::config::{self, Config};
use crate::error::Error;
use crate::paths::PathsInfo;
use crate::state::AppState;

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
