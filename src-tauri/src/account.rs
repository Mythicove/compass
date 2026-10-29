//! 账号：**机密与非机密分开存**，而且令牌**永不进 webview**。
//!
//! - `accounts.json`：身份与时间戳的列表（`AccountEntry`），前端能看到的就是这些。
//! - `secrets.json`：令牌（`Secrets`），0600 权限，前端永远拿不到。
//! - 前端拿到的是 `AccountView`（连 uuid 之外的东西都不给），所有需要令牌的调用
//!   都在 Rust 侧完成。这与 web 把 accessToken 关在 httpOnly cookie 里是同一个思路——
//!   只是桌面端没有 cookie，所以边界画在"命令返回值"上。
//!
//! **secrets.json 不是加密存储**，只是"分开存 + 收紧权限"。系统钥匙串（Windows
//! Credential Manager / macOS Keychain / Linux Secret Service）是计划里的目标，
//! 而"钥匙串不可用时怎么办"是一个还没定的决策（明文 + 明示风险 / 仅当次会话 /
//! 硬要求钥匙串三种都有道理），所以这里把落盘收敛到唯一一处：
//! 换成钥匙串时改 `load_secrets` / `put_secrets` / `delete_secrets` 就够了，
//! 调用方一行都不用动。`SecretStoreInfo.encrypted` 恒为 false，UI 必须据此明示，
//! 不能让人以为已经加密了——把混淆叫成加密，正是这个项目反复吃亏的那类错。

use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::paths::Paths;

/// 账号类型。微软正版（device code flow）还没接：它需要一个 Azure 应用的 client ID，
/// 那是个待定决策，所以这里先不放假的变体——加了就意味着 UI 上有个点了会失败的按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    /// 我们自己的镜像站（Yggdrasil）：玩家名 + 独立密码，令牌来自 `/authserver/authenticate`。
    /// 也是**缺省值**：P0–P3 期间存的账号文件里没有 `kind` 字段，而那时候只有这一种账号，
    /// 所以旧文件迁移过来必须落成 Yggdrasil 而不是某个"未知"。
    #[default]
    Yggdrasil,
    /// 离线模式：只有名字，UUID 由名字派生，没有令牌
    Offline,
}

impl AccountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AccountKind::Yggdrasil => "yggdrasil",
            AccountKind::Offline => "offline",
        }
    }
}

/// 启动时用的完整账号 = 非机密部分 + 从机密存储取回的令牌。
/// 这个结构体**只在 Rust 侧流转**，出去一律走 `view()`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    #[serde(default)]
    pub player_name: String,
    #[serde(default)]
    pub player_uuid: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub client_token: String,
    /// 登录时用的 Mirror 地址：换了站点旧令牌自然作废，不该跨站混用。
    #[serde(default)]
    pub api_url: String,
    /// Unix 秒。不用字符串是为了不引日期库，格式化交给前端。
    #[serde(default)]
    pub signed_in_at: u64,
    #[serde(default)]
    pub kind: AccountKind,
}

/// 交给前端的账号视图：**不含令牌**。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub player_name: String,
    pub player_uuid: String,
    pub api_url: String,
    pub signed_in_at: u64,
    pub kind: AccountKind,
}

impl Account {
    pub fn view(&self) -> AccountView {
        AccountView {
            player_name: self.player_name.clone(),
            player_uuid: self.player_uuid.clone(),
            api_url: self.api_url.clone(),
            signed_in_at: self.signed_in_at,
            kind: self.kind,
        }
    }
}

/// 账号列表里的一条：**不含任何机密**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountEntry {
    /// 本机内唯一，同时是机密存储的键。由 (kind, api_url, player_uuid) 派生，
    /// 不用随机数也不取玩家名：改名不该让令牌失联，重装也不该产生两个同一个人。
    pub id: String,
    pub kind: AccountKind,
    pub player_name: String,
    pub player_uuid: String,
    pub api_url: String,
    pub signed_in_at: u64,
    pub last_used_at: u64,
}

/// 机密部分。只存 `secrets.json`，永不进 accounts.json、永不进 webview。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Secrets {
    pub access_token: String,
    pub client_token: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountList {
    #[serde(default)]
    pub accounts: Vec<AccountEntry>,
    /// 当前使用的账号。None = 没有选中（未登录）
    #[serde(default)]
    pub active_id: Option<String>,
}

/// 机密存储的现状，给 UI 明示风险用。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStoreInfo {
    /// 目前只有 `"file"`；接了系统钥匙串之后会是 `"keyring"`
    pub kind: String,
    /// **恒为 false**：令牌是明文落盘的（权限 0600）。UI 必须照着说，
    /// 不能让人以为已经加密——见模块头。
    pub encrypted: bool,
    pub path: String,
}

/// 前端要的账号总览。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsView {
    pub accounts: Vec<AccountEntry>,
    pub active_id: Option<String>,
    pub store: SecretStoreInfo,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 离线模式的玩家 UUID：`UUID.nameUUIDFromBytes("OfflinePlayer:" + name)`，
/// 也就是对 UTF-8 字节做 MD5，再把版本位设成 3、变体位设成 IETF。
///
/// 必须与原版服务端、以及 mirror 的离线算法**逐字节一致**：不一致的后果是皮肤取不到、
/// 服务器把你认成另一个人，而症状离原因很远。期望值取自真 JDK 21 的
/// `UUID.nameUUIDFromBytes`（见 `offline_uuid_matches_the_java_implementation`），
/// 不是我照着算法描述自己算的——那样只能证明实现等于我对描述的理解。
pub fn offline_uuid(name: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(b"OfflinePlayer:");
    hasher.update(name.as_bytes());
    let mut d = hasher.finalize();
    // 版本 3（name-based, MD5）：byte 6 的高 4 位
    d[6] = (d[6] & 0x0f) | 0x30;
    // IETF 变体：byte 8 的高 2 位 = 10
    d[8] = (d[8] & 0x3f) | 0x80;
    hex::encode(d)
}

/// 账号 id：由身份派生，稳定且可重复计算。
pub fn account_id(kind: AccountKind, api_url: &str, player_uuid: &str) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(kind.as_str().as_bytes());
    h.update([0u8]);
    h.update(api_url.as_bytes());
    h.update([0u8]);
    h.update(player_uuid.as_bytes());
    hex::encode(h.finalize())[..16].to_string()
}

// ---------------------------------------------------------------------------
// 账号列表
// ---------------------------------------------------------------------------

/// 读账号列表。文件不存在 = 空列表；损坏则视为空，但**保留文件**并返回一条警告：
/// 静默删掉会把"我的账号怎么没了"的取证线索一起销毁。
///
/// 顺带做旧格式迁移：P0–P3 期间 accounts.json 里存的是**单个**账号、而且令牌就在
/// 里面。遇到那种文件就把它拆成"列表 + 机密"两份，并把令牌从 accounts.json 里抹掉。
pub fn load_list(paths: &Paths) -> (AccountList, Option<String>) {
    let Ok(text) = fs::read_to_string(paths.accounts_file()) else {
        return (AccountList::default(), None);
    };
    let unparsable = |e: String| {
        (
            AccountList::default(),
            Some(format!(
                "{} 无法解析，已视为未登录（文件原样保留以便排查）：{e}",
                paths.accounts_file().display()
            )),
        )
    };
    // 格式判定必须**看形状**，不能用"能不能解析成 AccountList"：
    // AccountList 两个字段都带 `#[serde(default)]`，于是任何 JSON 对象都能解析成一个
    // 空列表——旧格式会被静默当成"没有账号"，而令牌还留在文件里没人管。
    // 第一版就是这么写的，迁移测试立刻把它抓了出来。
    let shape: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return unparsable(e.to_string()),
    };
    let is_list = shape
        .as_object()
        .is_some_and(|o| o.get("accounts").is_some_and(|a| a.is_array()));
    if is_list {
        return match serde_json::from_value::<AccountList>(shape) {
            Ok(list) => (list, None),
            Err(e) => unparsable(e.to_string()),
        };
    }
    // 旧格式（P0–P3）：单个账号，令牌就在顶层
    if shape
        .as_object()
        .is_some_and(|o| o.contains_key("playerUuid"))
    {
        let legacy = match serde_json::from_value::<Account>(shape) {
            Ok(a) => a,
            Err(e) => {
                return (
                    AccountList::default(),
                    Some(format!("旧账号格式无法迁移（原文件已保留）：{e}")),
                );
            }
        };
        let mut list = AccountList::default();
        let id = match adopt(&mut list, &legacy) {
            Ok(id) => id,
            Err(e) => {
                return (
                    AccountList::default(),
                    Some(format!("旧账号格式无法迁移（原文件已保留）：{e}")),
                );
            }
        };
        let secrets = legacy_secrets(&legacy);
        list.active_id = Some(id.clone());
        // 迁移 = 写新列表 + 把令牌挪进机密存储。两步都成了才算迁移完，
        // 否则会出现"列表说是新格式，令牌却还在 accounts.json 里"
        if let Err(e) = save_list(paths, &list).and_then(|()| put_secrets(paths, &id, &secrets)) {
            return (
                AccountList::default(),
                Some(format!("旧账号格式迁移失败（原文件已保留）：{e}")),
            );
        }
        return (list, None);
    }
    (
        AccountList::default(),
        Some(format!(
            "{} 既不是账号列表也不是旧格式（原文件已保留以便排查）",
            paths.accounts_file().display()
        )),
    )
}

fn legacy_secrets(a: &Account) -> Secrets {
    Secrets {
        access_token: a.access_token.clone(),
        client_token: a.client_token.clone(),
    }
}

/// 把一个完整账号并入列表（同 id 则替换），返回它的 id。
fn adopt(list: &mut AccountList, account: &Account) -> Result<String, Error> {
    if account.player_uuid.trim().is_empty() {
        return Err(Error::Invalid("account has no player uuid".into()));
    }
    let id = account_id(account.kind, &account.api_url, &account.player_uuid);
    let now = now_unix();
    let entry = AccountEntry {
        id: id.clone(),
        kind: account.kind,
        player_name: account.player_name.clone(),
        player_uuid: account.player_uuid.clone(),
        api_url: account.api_url.clone(),
        signed_in_at: if account.signed_in_at == 0 {
            now
        } else {
            account.signed_in_at
        },
        last_used_at: now,
    };
    match list.accounts.iter_mut().find(|e| e.id == id) {
        Some(slot) => {
            // 登录时间只在真的重新登录时更新；名字可能改了（同一个 uuid 改名）
            slot.player_name = entry.player_name;
            slot.signed_in_at = entry.signed_in_at;
            slot.last_used_at = now;
        }
        None => list.accounts.push(entry),
    }
    Ok(id)
}

pub fn save_list(paths: &Paths, list: &AccountList) -> Result<(), Error> {
    write_json(&paths.accounts_file(), list)
}

/// 当前选中的账号（含令牌）。没有选中、或机密取不到时返回 None。
pub fn active(paths: &Paths) -> Option<Account> {
    let (list, _) = load_list(paths);
    let id = list.active_id.as_deref()?;
    let entry = list.accounts.iter().find(|e| e.id == id)?;
    let secrets = load_secrets(paths).remove(id).unwrap_or_default();
    Some(Account {
        player_name: entry.player_name.clone(),
        player_uuid: entry.player_uuid.clone(),
        access_token: secrets.access_token,
        client_token: secrets.client_token,
        api_url: entry.api_url.clone(),
        signed_in_at: entry.signed_in_at,
        kind: entry.kind,
    })
}

pub fn list_view(paths: &Paths) -> AccountsView {
    let (list, _) = load_list(paths);
    AccountsView {
        accounts: list.accounts,
        active_id: list.active_id,
        store: SecretStoreInfo {
            kind: "file".into(),
            encrypted: false,
            path: paths.secrets_file().to_string_lossy().into_owned(),
        },
    }
}

/// 保存（并设为当前）一个账号。令牌进机密存储，身份进列表。
pub fn save(paths: &Paths, account: &Account) -> Result<(), Error> {
    let (mut list, _) = load_list(paths);
    let id = adopt(&mut list, account)?;
    list.active_id = Some(id.clone());
    save_list(paths, &list)?;
    put_secrets(
        paths,
        &id,
        &Secrets {
            access_token: account.access_token.clone(),
            client_token: account.client_token.clone(),
        },
    )
}

/// 退出登录：删掉当前账号**与它的令牌**。
///
/// 只删当前那个，不动其它账号——多账号之后"退出"不该把别人的登录也清掉。
/// 列表空了才把两个文件一起删掉，这样"退出登录后磁盘上不残留可用令牌"
/// 是可以按文件断言的（见测试）。
pub fn clear(paths: &Paths) -> Result<(), Error> {
    let (mut list, _) = load_list(paths);
    let Some(id) = list.active_id.clone() else {
        // 没有当前账号：仍然把机密文件里可能残留的东西清掉，退出要彻底
        return remove_file_if_exists(&paths.accounts_file())
            .and_then(|()| remove_file_if_exists(&paths.secrets_file()));
    };
    delete_secrets(paths, &id)?;
    list.accounts.retain(|e| e.id != id);
    list.active_id = list.accounts.first().map(|e| e.id.clone());
    if list.accounts.is_empty() {
        return remove_file_if_exists(&paths.accounts_file())
            .and_then(|()| remove_file_if_exists(&paths.secrets_file()));
    }
    save_list(paths, &list)
}

/// 切换当前账号。目标不存在时报错——静默切到别的账号比报错更难查。
pub fn set_active(paths: &Paths, id: &str) -> Result<AccountEntry, Error> {
    let (mut list, _) = load_list(paths);
    let entry = list
        .accounts
        .iter()
        .find(|e| e.id == id)
        .cloned()
        .ok_or_else(|| Error::NotFound(format!("no such account: {id}")))?;
    list.active_id = Some(id.to_string());
    if let Some(slot) = list.accounts.iter_mut().find(|e| e.id == id) {
        slot.last_used_at = now_unix();
    }
    save_list(paths, &list)?;
    Ok(entry)
}

/// 删除一个账号（连同它的令牌）。幂等：不存在也算成功。
pub fn remove(paths: &Paths, id: &str) -> Result<(), Error> {
    let (mut list, _) = load_list(paths);
    delete_secrets(paths, id)?;
    list.accounts.retain(|e| e.id != id);
    if list.active_id.as_deref() == Some(id) {
        list.active_id = list.accounts.first().map(|e| e.id.clone());
    }
    if list.accounts.is_empty() {
        return remove_file_if_exists(&paths.accounts_file())
            .and_then(|()| remove_file_if_exists(&paths.secrets_file()));
    }
    save_list(paths, &list)
}

/// 加一个离线账号并设为当前。离线没有令牌，所以机密存储里不写东西。
pub fn add_offline(paths: &Paths, player_name: &str) -> Result<AccountEntry, Error> {
    let name = player_name.trim();
    if name.is_empty() || name.len() > 16 {
        return Err(Error::Invalid(format!(
            "离线玩家名要非空且不超过 16 字符：{player_name:?}"
        )));
    }
    // 原版对玩家名的限制：ASCII 字母数字与下划线。放行的话服务器会拒，
    // 而报错来自服务器，看不出是我们没拦
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::Invalid(format!(
            "离线玩家名只能用 ASCII 字母、数字与下划线：{player_name:?}"
        )));
    }
    let account = Account {
        player_name: name.to_string(),
        player_uuid: offline_uuid(name),
        access_token: String::new(),
        client_token: String::new(),
        api_url: String::new(),
        signed_in_at: now_unix(),
        kind: AccountKind::Offline,
    };
    save(paths, &account)?;
    let id = account_id(AccountKind::Offline, "", &account.player_uuid);
    let (list, _) = load_list(paths);
    list.accounts
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| Error::Config("offline account was not recorded".into()))
}

// ---------------------------------------------------------------------------
// 机密存储（换钥匙串时只改这三个函数）
// ---------------------------------------------------------------------------

pub fn load_secrets(paths: &Paths) -> HashMap<String, Secrets> {
    let Ok(text) = fs::read_to_string(paths.secrets_file()) else {
        return HashMap::new();
    };
    // 读不出来就当没有：机密文件坏了不该让整个启动器起不来，
    // 症状是"需要重新登录"，这比崩溃好
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn put_secrets(paths: &Paths, id: &str, secrets: &Secrets) -> Result<(), Error> {
    if id.is_empty() {
        return Err(Error::Invalid(
            "cannot store secrets without an account id".into(),
        ));
    }
    let mut map = load_secrets(paths);
    if secrets.access_token.is_empty() && secrets.client_token.is_empty() {
        // 离线账号没有令牌，不该在机密文件里留一条空记录
        map.remove(id);
    } else {
        map.insert(id.to_string(), secrets.clone());
    }
    write_json(&paths.secrets_file(), &map)
}

pub fn delete_secrets(paths: &Paths, id: &str) -> Result<(), Error> {
    let mut map = load_secrets(paths);
    if map.remove(id).is_none() {
        return Ok(());
    }
    if map.is_empty() {
        return remove_file_if_exists(&paths.secrets_file());
    }
    write_json(&paths.secrets_file(), &map)
}

// ---------------------------------------------------------------------------
// 兼容旧调用点
// ---------------------------------------------------------------------------

/// 读取当前账号（旧接口）。返回 (账号, 启动警告)。
pub fn load(paths: &Paths) -> (Option<Account>, Option<String>) {
    let (_, warning) = load_list(paths);
    (active(paths), warning)
}

fn write_json<T: Serialize>(file: &std::path::Path, value: &T) -> Result<(), Error> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(|e| Error::Config(e.to_string()))?;
    // 原子写：写一半断电不该留下半截 JSON
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, json)?;
    // 权限在 rename 之前就收紧：同机其他用户不该在任何时刻读到令牌
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, file)?;
    Ok(())
}

fn remove_file_if_exists(file: &std::path::Path) -> Result<(), Error> {
    match fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("data"));
        p.ensure().unwrap();
        (dir, p)
    }

    fn yggdrasil(name: &str, uuid: &str) -> Account {
        Account {
            player_name: name.into(),
            player_uuid: uuid.into(),
            access_token: format!("access-{uuid}"),
            client_token: format!("client-{uuid}"),
            api_url: "https://mirror.example.com".into(),
            signed_in_at: 1_700_000_000,
            kind: AccountKind::Yggdrasil,
        }
    }

    #[test]
    fn offline_uuid_matches_the_java_implementation() {
        // 期望值来自真 JDK 21 跑 `UUID.nameUUIDFromBytes(("OfflinePlayer:"+n).getBytes(UTF_8))`。
        // 这个算法必须与原版服务端一致，否则皮肤取不到、服务器认成另一个人，
        // 而症状离原因很远——所以不能"照着描述实现再自己测自己"。
        for (name, want) in [
            ("Test", "444cf323978c3e839288612345bfec67"),
            ("Steve", "5627dd98e6be3c21b8a8e92344183641"),
            ("Notch", "b50ad385829d3141a2167e7d7539ba7f"),
            ("", "fc5bc365aedf30a88b8904e462e29bde"),
            ("Hsukqi Lee", "1919c07399c43af49a358d58bafe316e"),
            ("élan", "47777009f7ae37568f6f3e5bd669246e"),
            ("aaaaaaaaaaaaaaaa", "7259babb8b7337de88fc86fa22252071"),
            ("中文名", "c956aba7f9753d37a1ff6c29d1a611c1"),
        ] {
            assert_eq!(offline_uuid(name), want, "name={name:?}");
        }
        // 版本位与变体位：v3 + IETF（版本位是 byte 6 的高 4 位 = 第 13 个十六进制字符）
        let u = offline_uuid("Steve");
        assert_eq!(&u[12..13], "3", "版本位应当是 3：{u}");
        let variant = u8::from_str_radix(&u[16..18], 16).unwrap();
        assert_eq!(variant & 0xc0, 0x80, "变体位应当是 IETF：{u}");
    }

    #[test]
    fn tokens_live_in_the_secrets_file_not_in_the_account_list() {
        let (_d, p) = paths();
        save(&p, &yggdrasil("Alice", "uuid-1")).unwrap();

        let list_text = fs::read_to_string(p.accounts_file()).unwrap();
        assert!(
            !list_text.contains("access-uuid-1"),
            "令牌出现在 accounts.json 里：{list_text}"
        );
        assert!(
            !list_text.contains("accessToken"),
            "accounts.json 里不该有令牌字段：{list_text}"
        );
        assert!(list_text.contains("Alice"));

        let secrets_text = fs::read_to_string(p.secrets_file()).unwrap();
        assert!(
            secrets_text.contains("access-uuid-1"),
            "令牌没进机密存储：{secrets_text}"
        );

        // 组装回来的账号仍然带着令牌（启动要用）
        let got = active(&p).unwrap();
        assert_eq!(got.access_token, "access-uuid-1");
        assert_eq!(got.player_name, "Alice");
        assert_eq!(got.kind, AccountKind::Yggdrasil);
        // 而给前端的视图里没有
        let view = serde_json::to_string(&got.view()).unwrap();
        assert!(!view.contains("access-uuid-1"), "view 泄露了令牌：{view}");
        assert!(view.contains("Alice"));
    }

    #[test]
    fn logout_leaves_no_usable_token_on_disk() {
        // 这是 Phase 4 的验收条件之一，而且是**按文件**断言的：
        // 不看接口返回什么，直接看磁盘上还剩什么。
        let (_d, p) = paths();
        save(&p, &yggdrasil("Alice", "uuid-1")).unwrap();
        assert!(p.secrets_file().exists());

        clear(&p).unwrap();
        assert!(load(&p).0.is_none(), "退出之后还读得到账号");
        assert!(
            !p.secrets_file().exists(),
            "机密文件还在：{}",
            p.secrets_file().display()
        );
        assert!(
            !p.accounts_file().exists(),
            "账号文件还在：{}",
            p.accounts_file().display()
        );
        // 数据目录里任何文件都不该还带着那个令牌
        let leftovers = scan_for(p.root(), "access-uuid-1");
        assert!(leftovers.is_empty(), "令牌残留在：{leftovers:?}");

        // 幂等
        clear(&p).unwrap();
    }

    #[test]
    fn logging_out_one_account_keeps_the_others() {
        let (_d, p) = paths();
        save(&p, &yggdrasil("Alice", "uuid-1")).unwrap();
        save(&p, &yggdrasil("Bob", "uuid-2")).unwrap();
        assert_eq!(active(&p).unwrap().player_name, "Bob");

        clear(&p).unwrap();
        // 只退出当前那个：多账号之后"退出"不该把别人的登录也清掉
        let got = active(&p).unwrap();
        assert_eq!(got.player_name, "Alice");
        assert_eq!(got.access_token, "access-uuid-1");
        let leftovers = scan_for(p.root(), "access-uuid-2");
        assert!(
            leftovers.is_empty(),
            "退出账号的令牌没被删干净：{leftovers:?}"
        );
    }

    #[test]
    fn multiple_accounts_can_be_switched_and_removed() {
        let (_d, p) = paths();
        save(&p, &yggdrasil("Alice", "uuid-1")).unwrap();
        save(&p, &yggdrasil("Bob", "uuid-2")).unwrap();
        add_offline(&p, "Steve").unwrap();

        let view = list_view(&p);
        assert_eq!(view.accounts.len(), 3, "{view:?}");
        assert_eq!(
            view.active_id.as_deref(),
            Some(account_id(AccountKind::Offline, "", &offline_uuid("Steve")).as_str())
        );
        assert_eq!(view.store.kind, "file");
        assert!(!view.store.encrypted, "现在就是明文落盘，不能谎报成已加密");
        // 前端拿到的列表里一个令牌都不该有
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("access-"), "列表泄露了令牌：{json}");

        // 切回 Alice
        let alice = account_id(
            AccountKind::Yggdrasil,
            "https://mirror.example.com",
            "uuid-1",
        );
        let entry = set_active(&p, &alice).unwrap();
        assert_eq!(entry.player_name, "Alice");
        assert_eq!(active(&p).unwrap().access_token, "access-uuid-1");
        // 离线账号没有令牌，但一样能用来启动
        let steve = account_id(AccountKind::Offline, "", &offline_uuid("Steve"));
        set_active(&p, &steve).unwrap();
        let got = active(&p).unwrap();
        assert_eq!(got.kind, AccountKind::Offline);
        assert_eq!(got.access_token, "", "离线账号不该有令牌");
        assert_eq!(got.player_uuid, offline_uuid("Steve"));

        // 切换到一个不存在的 id 必须报错，不能静默停在原账号
        assert!(set_active(&p, "nope").is_err());

        // 删掉当前账号后自动落到剩下的第一个，且令牌一起删
        remove(&p, &steve).unwrap();
        let after = load_list(&p).0;
        assert_eq!(after.accounts.len(), 2);
        assert_ne!(after.active_id.as_deref(), Some(steve.as_str()));
        let leftovers = scan_for(p.root(), &offline_uuid("Steve"));
        // 离线 uuid 本身不是机密，列表里还留着别人的；这里只验它不再是当前账号
        let _ = leftovers;
    }

    #[test]
    fn offline_accounts_reject_names_the_server_would_reject() {
        let (_d, p) = paths();
        for bad in ["", "   ", "名字", "a b", "a-b", &"x".repeat(17)] {
            assert!(
                add_offline(&p, bad).is_err(),
                "应当拒绝 {bad:?}：服务器会拒，而那句报错来自服务器，看不出是我们没拦"
            );
        }
        let e = add_offline(&p, "Steve_01").unwrap();
        assert_eq!(e.player_uuid, offline_uuid("Steve_01"));
        assert_eq!(e.kind, AccountKind::Offline);
        // 前后有空格要 trimming 后再派生 uuid，否则同一个名字会有两个 uuid
        let e2 = add_offline(&p, "  Steve_01  ").unwrap();
        assert_eq!(e2.id, e.id, "同一个人不该变成两个账号");
    }

    #[test]
    fn the_old_single_account_file_is_migrated_and_its_token_removed() {
        let (_d, p) = paths();
        // P0–P3 的格式：单个账号，令牌就在 accounts.json 里
        let legacy = r#"{
            "playerName": "Alice", "playerUuid": "uuid-1",
            "accessToken": "legacy-token", "clientToken": "legacy-client",
            "apiUrl": "https://mirror.example.com", "signedInAt": 1700000000
        }"#;
        fs::write(p.accounts_file(), legacy).unwrap();

        let got = load(&p).0.expect("旧账号应当被迁移而不是丢掉");
        assert_eq!(got.player_name, "Alice");
        assert_eq!(got.access_token, "legacy-token");
        // kind 缺省为 yggdrasil：旧格式只有那一种账号
        assert_eq!(got.kind, AccountKind::Yggdrasil);

        let list_text = fs::read_to_string(p.accounts_file()).unwrap();
        assert!(
            !list_text.contains("legacy-token"),
            "迁移之后令牌还留在 accounts.json 里：{list_text}"
        );
        assert!(list_text.contains("\"accounts\""), "{list_text}");
        let secrets_text = fs::read_to_string(p.secrets_file()).unwrap();
        assert!(secrets_text.contains("legacy-token"), "{secrets_text}");

        // 迁移是幂等的：再读一次不该重复添加
        let again = load_list(&p).0;
        assert_eq!(again.accounts.len(), 1, "{again:?}");
    }

    #[test]
    fn corrupt_files_are_kept_and_reported_not_deleted() {
        let (_d, p) = paths();
        fs::write(p.accounts_file(), "{ nope").unwrap();
        let (account, warning) = load(&p);
        assert!(account.is_none());
        // 视为未登录，但文件保留（取证）、警告走 startup_warning 机制
        assert!(p.accounts_file().exists());
        assert!(
            warning.as_ref().is_some_and(|w| w.contains("无法解析")),
            "{warning:?}"
        );

        // 机密文件坏了也不该让启动器起不来：症状是"需要重新登录"
        fs::write(p.secrets_file(), "{ nope").unwrap();
        assert!(load_secrets(&p).is_empty());
    }

    #[test]
    fn the_same_account_saved_twice_does_not_duplicate() {
        let (_d, p) = paths();
        save(&p, &yggdrasil("Alice", "uuid-1")).unwrap();
        let mut again = yggdrasil("Alice", "uuid-1");
        again.access_token = "rotated".into();
        save(&p, &again).unwrap();
        let list = load_list(&p).0;
        assert_eq!(list.accounts.len(), 1, "{list:?}");
        // 令牌轮换要跟上，否则拿着过期令牌去启动
        assert_eq!(active(&p).unwrap().access_token, "rotated");
        let leftovers = scan_for(p.root(), "access-uuid-1");
        assert!(
            leftovers.is_empty(),
            "轮换之后旧令牌还留在磁盘上：{leftovers:?}"
        );
    }

    #[test]
    fn account_ids_are_stable_and_do_not_collide_across_sites() {
        // id 由 (kind, api_url, uuid) 派生：同一个人在两个镜像站上是两个账号
        // （令牌本来就不通用），而同一个人重复登录不会产生第二条
        let a = account_id(AccountKind::Yggdrasil, "https://a.example", "uuid-1");
        let b = account_id(AccountKind::Yggdrasil, "https://b.example", "uuid-1");
        assert_ne!(a, b, "不同站点的同 uuid 不该合并成一个账号");
        assert_eq!(
            a,
            account_id(AccountKind::Yggdrasil, "https://a.example", "uuid-1")
        );
        // 类型也要进 id：离线与镜像站的同名玩家不是一个人
        assert_ne!(
            a,
            account_id(AccountKind::Offline, "https://a.example", "uuid-1")
        );
        assert_eq!(a.len(), 16);
    }

    /// 在目录树里找还带着某段内容的文件（"退出后不残留"这类断言用）。
    fn scan_for(root: &std::path::Path, needle: &str) -> Vec<String> {
        let mut hits = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(text) = fs::read_to_string(&path)
                    && text.contains(needle)
                {
                    hits.push(path.to_string_lossy().into_owned());
                }
            }
        }
        hits
    }
}
