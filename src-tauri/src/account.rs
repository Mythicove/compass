//! 已登录账号。令牌只存在数据目录里（P4 迁到系统钥匙串），**永不进 webview**：
//! 前端拿到的是 `AccountView`（无令牌），所有需要令牌的调用都在 Rust 侧完成。
//! 这与 web 把 accessToken 关在 httpOnly cookie 里是同一个思路——只是桌面端没有 cookie，
//! 所以边界画在"命令返回值"上。

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::paths::Paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub player_name: String,
    pub player_uuid: String,
    pub access_token: String,
    pub client_token: String,
    /// 登录时用的 Mirror 地址：换了站点旧令牌自然作废，不该跨站混用。
    pub api_url: String,
    /// Unix 秒。不用字符串是为了不引日期库，格式化交给前端。
    pub signed_in_at: u64,
}

/// 交给前端的账号视图：**不含令牌**。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub player_name: String,
    pub player_uuid: String,
    pub api_url: String,
    pub signed_in_at: u64,
}

impl Account {
    pub fn view(&self) -> AccountView {
        AccountView {
            player_name: self.player_name.clone(),
            player_uuid: self.player_uuid.clone(),
            api_url: self.api_url.clone(),
            signed_in_at: self.signed_in_at,
        }
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 读取已登录账号。文件不存在 = 未登录；损坏则视为未登录并**删掉**这个文件：
/// 账号状态是可重建的（重新登录即可），不该让一个坏文件把启动器卡在错误页上。
pub fn load(paths: &Paths) -> Option<Account> {
    let text = fs::read_to_string(paths.accounts_file()).ok()?;
    match serde_json::from_str::<Account>(&text) {
        Ok(a) => Some(a),
        Err(_) => {
            let _ = fs::remove_file(paths.accounts_file());
            None
        }
    }
}

pub fn save(paths: &Paths, account: &Account) -> Result<(), Error> {
    if let Some(parent) = paths.accounts_file().parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(account).map_err(|e| Error::Config(e.to_string()))?;
    // 原子写：与 compass.json 同一套理由（写一半断电不该留下半截 JSON）
    let tmp = paths.accounts_file().with_extension("json.tmp");
    fs::write(&tmp, json)?;
    fs::rename(&tmp, paths.accounts_file())?;
    Ok(())
}

pub fn clear(paths: &Paths) -> Result<(), Error> {
    match fs::remove_file(paths.accounts_file()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Account {
        Account {
            player_name: "Alice".into(),
            player_uuid: "uuid-1".into(),
            access_token: "secret-access".into(),
            client_token: "secret-client".into(),
            api_url: "https://mirror.example.com".into(),
            signed_in_at: 1_700_000_000,
        }
    }

    #[test]
    fn roundtrip_preserves_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        paths.ensure().unwrap();
        assert!(load(&paths).is_none());
        save(&paths, &sample()).unwrap();
        let got = load(&paths).unwrap();
        assert_eq!(got.access_token, "secret-access");
        assert_eq!(got.player_name, "Alice");
    }

    #[test]
    fn view_never_leaks_tokens() {
        let json = serde_json::to_string(&sample().view()).unwrap();
        assert!(!json.contains("secret"), "view leaked a token: {json}");
        assert!(json.contains("Alice"));
    }

    #[test]
    fn corrupt_file_is_dropped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        paths.ensure().unwrap();
        fs::write(paths.accounts_file(), "{ nope").unwrap();
        assert!(load(&paths).is_none());
        assert!(!paths.accounts_file().exists());
    }

    #[test]
    fn clear_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        paths.ensure().unwrap();
        clear(&paths).unwrap();
        save(&paths, &sample()).unwrap();
        clear(&paths).unwrap();
        assert!(load(&paths).is_none());
    }
}
