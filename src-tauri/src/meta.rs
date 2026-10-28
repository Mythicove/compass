//! 版本清单与元数据获取。
//!
//! 解析是纯函数（对着真实清单样本做契约测试）；网络请求统一经 `net::rewrite_mirror`，
//! 于是"换镜像"对清单、版本 JSON、资源、库**全都**生效，不需要每个模块各写一遍。

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::api::api_error;
use crate::assets::AssetIndex;
use crate::error::Error;
use crate::net::rewrite_mirror;
use crate::version::VersionJson;

pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub release_time: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub compliance_level: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionManifest {
    pub latest: Latest,
    pub versions: Vec<ManifestEntry>,
}

impl VersionManifest {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn by_id(&self, id: &str) -> Option<&ManifestEntry> {
        self.versions.iter().find(|v| v.id == id)
    }

    /// 正式版（安装界面的默认视图；快照要单独开关，否则列表里全是噪音）。
    pub fn releases(&self) -> impl Iterator<Item = &ManifestEntry> {
        self.versions.iter().filter(|v| v.kind == "release")
    }
}

/// 继承链推进**一步**：把父版本并进子版本，返回 `(合并结果, 下一个要解析的父 id)`。
///
/// 两件事必须同时成立，缺一个就出 bug（两个都真出过）：
///
/// 1. 合并结果**仍然要声明** `inheritsFrom`，指向刚并进来的那个父版本。
///    `VersionJson::jar_version()` 靠它定位 client jar——继承型版本自己不带 jar，
///    jar 在父版本的目录下。清掉它，装完的 Fabric 会去找
///    `versions/fabric-loader-…/fabric-loader-….jar`，症状是启动时"client jar missing"。
/// 2. 链的推进要用**父版本自己的** `inheritsFrom`（返回值里的第二个元素），
///    不能读合并结果的。`version::merge` 是"子覆盖父"，合并结果里的 `inheritsFrom`
///    永远是子版本原来那个值，读它就等于把同一个父版本反复解析，
///    一直撞到深度上限（真装 Fabric 时暴露的：原版没有 `inheritsFrom`，
///    这条循环在 P1 从没被真跑过）。
///
/// 抽成纯函数是为了能离线测：`resolve_from` 要联网，而这两个 bug 都在这一步里。
pub fn merge_step(parent: VersionJson, child: VersionJson) -> (VersionJson, Option<String>) {
    let next = parent.inherits_from.clone();
    let parent_id = parent.id.clone();
    let mut merged = crate::version::merge(parent, child);
    merged.inherits_from = Some(parent_id);
    (merged, next)
}

/// 元数据客户端：只读 JSON，不落盘（缓存由调用方按 `paths` 布局决定）。
#[derive(Clone)]
pub struct MetaClient {
    http: reqwest::Client,
    mirror: String,
}

impl MetaClient {
    pub fn new(http: reqwest::Client, mirror: impl Into<String>) -> Self {
        Self {
            http,
            mirror: mirror.into(),
        }
    }

    pub async fn json<T: DeserializeOwned>(&self, url: &str) -> Result<T, Error> {
        let text = self.text(url).await?;
        serde_json::from_str(&text).map_err(|e| Error::Config(format!("decode {url}: {e}")))
    }

    pub async fn text(&self, url: &str) -> Result<String, Error> {
        let res = self
            .http
            .get(rewrite_mirror(url, &self.mirror))
            .send()
            .await?;
        let status = res.status().as_u16();
        let body = res.text().await?;
        if !(200..300).contains(&status) {
            return Err(api_error(status, &body));
        }
        Ok(body)
    }

    pub async fn manifest(&self) -> Result<VersionManifest, Error> {
        self.json(VERSION_MANIFEST_URL).await
    }

    /// 版本 JSON 可能是继承型的（Fabric/Forge），此时还要取父版本再合并。
    pub async fn version(&self, url: &str) -> Result<VersionJson, Error> {
        self.json(url).await
    }

    /// 取清单条目的版本 JSON，**先对字节做 sha1 校验再解析**：
    /// 清单给了 sha1 却不核对，等于允许镜像/中间人替换版本 JSON——它决定我们
    /// 下载什么、把什么放进 classpath，是整条安装链的信任根。
    pub async fn version_for(&self, entry: &ManifestEntry) -> Result<VersionJson, Error> {
        let url = rewrite_mirror(&entry.url, &self.mirror);
        let res = self.http.get(&url).send().await?;
        let status = res.status().as_u16();
        let bytes = res.bytes().await?;
        if !(200..300).contains(&status) {
            return Err(api_error(status, &String::from_utf8_lossy(&bytes)));
        }
        if !entry.sha1.is_empty() {
            let mut hasher = Sha1::new();
            hasher.update(&bytes);
            let actual = hex::encode(hasher.finalize());
            if actual != entry.sha1 {
                return Err(Error::Checksum(format!(
                    "{}: want {} got {actual}",
                    entry.url, entry.sha1
                )));
            }
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::Config(format!("decode {}: {e}", entry.url)))
    }

    /// 递归解析继承链：子版本在前，返回合并后的版本。
    /// 深度上限防环——loader 生成的 JSON 正常只有一层，但坏数据不该让启动器无限递归。
    pub async fn resolve_version(&self, entry: &ManifestEntry) -> Result<VersionJson, Error> {
        let child = self.version_for(entry).await?;
        self.resolve_from(child).await
    }

    /// 从一个**已经拿到的**子版本 JSON 开始解析继承链。
    ///
    /// loader 的 profile 不在官方清单里（没有清单条目可传、也没有 sha1 可核对），
    /// 所以要有这个入口；但父版本仍然从清单取并校验 sha1——信任根不变。
    /// 链的推进规则见 `merge_step`（那里记着两个真出过的 bug）。
    pub async fn resolve_from(&self, child: VersionJson) -> Result<VersionJson, Error> {
        const MAX_DEPTH: usize = 8;
        let root_id = child.id.clone();
        let mut merged = child;
        let mut next = merged.inherits_from.clone();
        for _ in 0..MAX_DEPTH {
            let Some(parent_id) = next.take() else {
                return Ok(merged);
            };
            let manifest = self.manifest().await?;
            let parent = manifest
                .by_id(&parent_id)
                .ok_or_else(|| Error::Invalid(format!("unknown parent version: {parent_id}")))?;
            let parent_json = self.version_for(parent).await?;
            (merged, next) = merge_step(parent_json, merged);
        }
        Err(Error::Invalid(format!(
            "version inheritance deeper than {MAX_DEPTH}: {root_id}"
        )))
    }

    pub async fn asset_index(&self, url: &str) -> Result<AssetIndex, Error> {
        self.json(url).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trimmed() -> VersionManifest {
        VersionManifest::parse(include_str!(
            "../tests/fixtures/version_manifest_v2-trimmed.json"
        ))
        .unwrap()
    }

    #[test]
    fn parses_real_manifest() {
        let m = trimmed();
        assert!(!m.versions.is_empty());
        // latest 指向的版本必须真的在列表里（否则"安装最新版"会拿到 None）
        assert!(m.by_id(&m.latest.release).is_some());
        assert!(m.by_id(&m.latest.snapshot).is_some());
        let entry = m.by_id("1.21.4").unwrap();
        assert_eq!(entry.kind, "release");
        assert!(entry.url.ends_with("/1.21.4.json"));
        assert_eq!(entry.sha1.len(), 40);
        assert!(!entry.release_time.is_empty());
    }

    #[test]
    fn releases_filter_out_snapshots() {
        let m = trimmed();
        let releases: Vec<_> = m.releases().map(|v| v.id.clone()).collect();
        assert!(releases.contains(&"1.21.4".to_string()));
        assert!(releases.contains(&"1.7.10".to_string()));
        assert!(
            !releases.iter().any(|id| id.contains("snapshot")),
            "snapshots must not show up as releases: {releases:?}"
        );
        assert!(m.versions.len() > releases.len());
    }

    #[test]
    fn merge_step_keeps_the_pointer_to_the_version_that_provides_the_jar() {
        // 真实样本：Fabric 的 profile（inheritsFrom 1.21.4）+ 1.21.4 本身
        let child =
            VersionJson::parse(include_str!("../tests/fixtures/fabric-1.21.4.json")).unwrap();
        let parent = VersionJson::parse(include_str!("../tests/fixtures/1.21.4.json")).unwrap();
        assert_eq!(child.inherits_from.as_deref(), Some("1.21.4"));
        assert!(parent.inherits_from.is_none());

        let (merged, next) = merge_step(parent, child);
        // 链在这里走完：父版本是根，没有下一个
        assert_eq!(next, None, "根版本不该再往上指");
        // 合并结果仍然指向提供 client jar 的版本——jar_version() 靠它定位。
        // 清掉它的话，装完的 Fabric 会去 versions/fabric-loader-…/ 找 jar（不存在）。
        assert_eq!(merged.inherits_from.as_deref(), Some("1.21.4"));
        assert_eq!(
            merged.jar_version(),
            "1.21.4",
            "client jar 来自父版本（继承型版本自己不带 jar）"
        );
        // 合并本身没被弄坏：主类来自子版本，id 也是子版本的（装出来的目录名就是它）
        assert!(
            merged
                .main_class
                .as_deref()
                .is_some_and(|m| m.contains("fabricmc")),
            "{:?}",
            merged.main_class
        );
        // 断言形状而不是具体版本号：写死 "0.16.9" 的话，刷新 fixture 就会假失败
        assert!(merged.id.starts_with("fabric-loader-"), "{}", merged.id);
        assert!(merged.id.ends_with("-1.21.4"), "{}", merged.id);
    }

    #[test]
    fn merge_step_walks_a_multi_level_chain_one_step_at_a_time() {
        // A inherits B，B inherits C，C 是根。每调用一次只推进一层，
        // 推进的依据是**父版本自己的**指针（返回值第二项），不是合并结果里的。
        let a = VersionJson::parse(r#"{"id":"a","inheritsFrom":"b","mainClass":"A"}"#).unwrap();
        let b = VersionJson::parse(r#"{"id":"b","inheritsFrom":"c"}"#).unwrap();
        let c = VersionJson::parse(r#"{"id":"c","assets":"19"}"#).unwrap();

        let (after_b, next) = merge_step(b, a);
        assert_eq!(next.as_deref(), Some("c"), "下一步该去解析 c");
        assert_eq!(
            after_b.main_class.as_deref(),
            Some("A"),
            "子版本的字段要保住"
        );

        let (after_c, next) = merge_step(c, after_b);
        assert_eq!(next, None, "到根就该停下");
        assert_eq!(after_c.main_class.as_deref(), Some("A"));
        assert_eq!(
            after_c.assets.as_deref(),
            Some("19"),
            "根版本的字段要合并进来"
        );
        assert_eq!(after_c.jar_version(), "c", "jar 由链最上层的那个版本提供");
    }

    #[test]
    fn resolving_a_real_chain_terminates() {
        // 模拟 resolve_from 的循环（不联网）：真实的两层链必须一步收敛。
        // 之前读的是合并结果里的 inheritsFrom，于是原地打转跑满 MAX_DEPTH，
        // 报 "version inheritance deeper than 8"。
        let mut merged =
            VersionJson::parse(include_str!("../tests/fixtures/fabric-1.21.4.json")).unwrap();
        let parent = VersionJson::parse(include_str!("../tests/fixtures/1.21.4.json")).unwrap();
        let mut next = merged.inherits_from.clone();
        let mut steps = 0;
        while next.take().is_some() {
            (merged, next) = merge_step(parent.clone(), merged);
            steps += 1;
            assert!(steps <= 8, "继承链没收敛（跑满 8 步还在往上指）");
        }
        assert_eq!(steps, 1, "Fabric 的链只有一层，应当一步收敛");
        assert_eq!(merged.jar_version(), "1.21.4");
    }

    #[test]
    fn unknown_version_is_none() {
        assert!(trimmed().by_id("99.99.99").is_none());
    }
}
