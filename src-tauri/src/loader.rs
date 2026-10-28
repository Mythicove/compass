//! Mod loader 安装：Fabric 与 Quilt。
//!
//! 两家 meta 服务的形状一致（Quilt 的 meta 是 Fabric 的分叉），端点也同构：
//!
//! - `{base}/versions/loader/{game}` → 该游戏版本可用的 loader 版本列表
//! - `{base}/versions/loader/{game}/{loader}/profile/json` → 继承型版本 JSON
//!
//! 所以一套实现、两个 base 就够。profile 的 `inheritsFrom` 指向游戏版本，
//! 于是合并继承链与下载全部复用 P1 那条链路（`Installer::install_profile`）——
//! 装 loader 与装原版走的是同一段代码，只是子版本 JSON 的来源不同。
//!
//! 上面这些端点与字段是**对着线上服务核过的**（2026-10-28），不是照文档想象写的：
//! Fabric 的库条目把 `sha1`/`size` 放在库对象上、`url` 只是 Maven 基址；
//! Quilt 的部分库条目连校验和都没有（空 sha1 = 无从校验，不等于校验通过）。
//! 两者 P1 的 `version.rs` 已经能处理。

use serde::{Deserialize, Serialize};

use crate::api::{ApiClient, join_path};
use crate::error::Error;
use crate::install::{InstalledVersion, Installer};
use crate::net::ProgressSink;
use crate::version::{self, VersionJson};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    Fabric,
    Quilt,
}

impl LoaderKind {
    pub const ALL: [LoaderKind; 2] = [LoaderKind::Fabric, LoaderKind::Quilt];

    /// meta 服务的根。**API 版本号是路径的一部分**（Fabric 用 v2、Quilt 用 v3），
    /// 不是我们能挑的，所以写死在这里而不是拼出来。
    pub fn meta_base(self) -> &'static str {
        match self {
            LoaderKind::Fabric => "https://meta.fabricmc.net/v2",
            LoaderKind::Quilt => "https://meta.quiltmc.org/v3",
        }
    }

    pub fn parse(s: &str) -> Result<LoaderKind, Error> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| Error::Invalid(format!("unsupported loader: {s:?}")))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LoaderKind::Fabric => "fabric",
            LoaderKind::Quilt => "quilt",
        }
    }
}

/// 一个可用的 loader 版本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoaderVersion {
    pub version: String,
    /// `None` = 来源没说。**不能当成 false**：Fabric 的 loader 对象有 `stable`，
    /// Quilt 的根本没有这个字段；把"没写"显示成"开发版"是替它编了一句它没说过的话。
    pub stable: Option<bool>,
}

#[derive(Deserialize)]
struct LoaderEntry {
    loader: LoaderInfo,
}

#[derive(Deserialize)]
struct LoaderInfo {
    version: String,
    #[serde(default)]
    stable: Option<bool>,
}

/// 列出某个游戏版本可用的 loader 版本。
///
/// 顺序自己排：实测 Fabric 大致是最新在前，Quilt 返回的是
/// `0.20.0-beta.9, 0.20.0-beta.7, 0.20.0-beta.8, …`——把这种顺序直接给用户，
/// 看起来像列表在乱跳。stable 优先，其次按版本号倒序；
/// 排不出结果的（版本号不是数字）保持服务给的相对顺序。
pub async fn versions(
    api: &ApiClient,
    kind: LoaderKind,
    game: &str,
) -> Result<Vec<LoaderVersion>, Error> {
    let url = join_path(kind.meta_base(), &["versions", "loader", game])?;
    let entries: Vec<LoaderEntry> = api.get_json(&url).await?;
    let mut out: Vec<LoaderVersion> = entries
        .into_iter()
        .map(|e| LoaderVersion {
            version: e.loader.version,
            stable: e.loader.stable,
        })
        .filter(|v| !v.version.trim().is_empty())
        .collect();
    sort_versions(&mut out);
    Ok(out)
}

/// 排序规则**只有一处**：stable 优先，其次版本号倒序。
/// 测试调的也是这个函数——测试里再写一遍比较器的话，它验的是自己那份副本。
fn sort_versions(out: &mut [LoaderVersion]) {
    out.sort_by(|a, b| {
        stable_rank(a)
            .cmp(&stable_rank(b))
            .then_with(|| version_key(&b.version).cmp(&version_key(&a.version)))
    });
}

fn stable_rank(v: &LoaderVersion) -> u8 {
    match v.stable {
        Some(true) => 0,
        // 没说的排在"明确 stable"之后、"明确不是"之前
        None => 1,
        Some(false) => 2,
    }
}

/// 版本号排序键。`version_parts` 只认三段数字（`0.20.0-beta.9` → `(0,20,0)`），
/// 预发布后缀会并列；并列时靠 `sort_by` 的稳定性保持服务给的顺序。
fn version_key(v: &str) -> (u32, u32, u32) {
    version::version_parts(v).unwrap_or((0, 0, 0))
}

/// 取 loader 的 profile（一个 `inheritsFrom` 游戏版本的版本 JSON）。
pub async fn profile(
    api: &ApiClient,
    kind: LoaderKind,
    game: &str,
    loader: &str,
) -> Result<VersionJson, Error> {
    let url = join_path(
        kind.meta_base(),
        &["versions", "loader", game, loader, "profile", "json"],
    )?;
    let v: VersionJson = api.get_json(&url).await?;
    require_inherited(v, kind, game, loader)
}

/// profile 必须声明继承。少了它，合并出来就没有 client jar 与资源索引，
/// 而失败会出现在"启动时缺文件"——比现在这句难查得多。
fn require_inherited(
    v: VersionJson,
    kind: LoaderKind,
    game: &str,
    loader: &str,
) -> Result<VersionJson, Error> {
    if v.inherits_from.is_none() {
        return Err(Error::Config(format!(
            "{} profile for {game}/{loader} has no inheritsFrom",
            kind.as_str()
        )));
    }
    Ok(v)
}

/// 装一个 loader 版本：取 profile → 与父版本合并 → 下载库与资源。
///
/// 返回的 `InstalledVersion.id` 是 profile 给的 id（如 `fabric-loader-0.16.9-1.21.4`），
/// 实例的 `version_id` 应当存它——那才是启动时真正用的版本。
pub async fn install(
    installer: &Installer,
    api: &ApiClient,
    kind: LoaderKind,
    game: &str,
    loader: &str,
    progress: Option<ProgressSink>,
) -> Result<InstalledVersion, Error> {
    let child = profile(api, kind, game, loader).await?;
    installer.install_profile(child, progress).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_serialize_as_the_names_the_ui_uses() {
        for k in LoaderKind::ALL {
            assert_eq!(
                serde_json::to_string(&k).unwrap(),
                format!("\"{}\"", k.as_str())
            );
            assert_eq!(LoaderKind::parse(k.as_str()).unwrap(), k);
        }
        assert!(
            LoaderKind::parse("forge").is_err(),
            "Forge 是 P3 后半段的事"
        );
        assert!(
            LoaderKind::parse("Fabric").is_err(),
            "大小写敏感：取值就是 UI 传的字符串"
        );
        // base 里的 API 版本号是各家自己定的，不是我们能推的
        assert_eq!(
            LoaderKind::Fabric.meta_base(),
            "https://meta.fabricmc.net/v2"
        );
        assert_eq!(LoaderKind::Quilt.meta_base(), "https://meta.quiltmc.org/v3");
    }

    #[test]
    fn loader_urls_are_built_from_escaped_segments() {
        // 游戏版本与 loader 版本都来自 UI/服务端，直接内插会让 `../`、`?`、`#` 改写请求目标
        let fabric = join_path(
            LoaderKind::Fabric.meta_base(),
            &["versions", "loader", "1.21.4", "0.16.9", "profile", "json"],
        )
        .unwrap();
        assert_eq!(
            fabric,
            "https://meta.fabricmc.net/v2/versions/loader/1.21.4/0.16.9/profile/json"
        );
        let quilt = join_path(
            LoaderKind::Quilt.meta_base(),
            &["versions", "loader", "1.21.4"],
        )
        .unwrap();
        assert_eq!(quilt, "https://meta.quiltmc.org/v3/versions/loader/1.21.4");

        let evil = join_path(
            LoaderKind::Fabric.meta_base(),
            &["versions", "loader", "../../x"],
        )
        .unwrap();
        assert!(
            evil.contains("%2F") || !evil.contains("/x"),
            "分隔符必须被编码，不能真的跳到别的路径：{evil}"
        );
    }

    #[test]
    fn parses_the_real_meta_response_shapes() {
        // Fabric：loader 对象带 stable（实测字段 separator/build/maven/version/stable）
        let fabric = r#"[
            {"loader":{"separator":".","build":1,"maven":"net.fabricmc:fabric-loader:0.20.1","version":"0.20.1","stable":true},
             "intermediary":{"maven":"net.fabricmc:intermediary:1.21.4","version":"1.21.4","stable":true},
             "launcherMeta":{"version":2,"min_java_version":8}},
            {"loader":{"version":"0.20.0","stable":false},"intermediary":{"version":"1.21.4"},"launcherMeta":{}}
        ]"#;
        let entries: Vec<LoaderEntry> = serde_json::from_str(fabric).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].loader.version, "0.20.1");
        assert_eq!(entries[0].loader.stable, Some(true));
        assert_eq!(entries[1].loader.stable, Some(false));

        // Quilt：loader 对象**没有** stable，多的是 hashed 与 hashes（实测字段）
        let quilt = r#"[
            {"loader":{"maven":"org.quiltmc:quilt-loader:0.20.0-beta.9","version":"0.20.0-beta.9",
                       "build":9,"separator":".","file_size":1550096,
                       "hashes":{"sha1":"bed0a01b","sha256":"820b1880"}},
             "hashed":{"maven":"org.quiltmc:hashed:1.21.4","version":"1.21.4"},
             "intermediary":{"maven":"net.fabricmc:intermediary:1.21.4","version":"1.21.4"},
             "launcherMeta":{}}
        ]"#;
        let q: Vec<LoaderEntry> = serde_json::from_str(quilt).unwrap();
        assert_eq!(q[0].loader.version, "0.20.0-beta.9");
        assert_eq!(
            q[0].loader.stable, None,
            "Quilt 没有 stable 字段，必须是 None 而不是 false"
        );
    }

    #[test]
    fn sorts_stable_first_then_newest() {
        let mut v = [
            LoaderVersion {
                version: "0.19.5".into(),
                stable: Some(false),
            },
            LoaderVersion {
                version: "0.20.0-beta.9".into(),
                stable: None,
            },
            LoaderVersion {
                version: "0.20.1".into(),
                stable: Some(true),
            },
            LoaderVersion {
                version: "0.20.0".into(),
                stable: Some(false),
            },
        ];
        sort_versions(&mut v);
        let got: Vec<&str> = v.iter().map(|x| x.version.as_str()).collect();
        assert_eq!(
            got,
            vec!["0.20.1", "0.20.0-beta.9", "0.20.0", "0.19.5"],
            "{got:?}"
        );
    }

    #[test]
    fn profile_without_inherits_from_is_rejected() {
        // 少了 inheritsFrom 的话，合并出来没有 client jar 与资源索引，
        // 症状会是"启动时缺文件"——所以在这里就拒绝，并说清是哪个 profile
        let text = r#"{"id":"fabric-loader-0.16.9-1.21.4","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient"}"#;
        let v = VersionJson::parse(text).unwrap();
        let err = require_inherited(v, LoaderKind::Fabric, "1.21.4", "0.16.9").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("inheritsFrom"), "{msg}");
        assert!(msg.contains("fabric"), "{msg}");
        assert!(msg.contains("1.21.4/0.16.9"), "{msg}");

        // 有 inheritsFrom 就放行（真实 profile 的形状见 tests/loader_live.rs）
        let ok =
            VersionJson::parse(r#"{"id":"fabric-loader-0.16.9-1.21.4","inheritsFrom":"1.21.4"}"#)
                .unwrap();
        assert!(require_inherited(ok, LoaderKind::Fabric, "1.21.4", "0.16.9").is_ok());
    }
}
