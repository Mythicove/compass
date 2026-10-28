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

    /// 递归解析继承链：子版本在前，返回 `(已合并的版本, 链上每个版本的原始 JSON 顺序)`。
    /// 深度上限防环——loader 生成的 JSON 正常只有一层，但坏数据不该让启动器无限递归。
    pub async fn resolve_version(&self, entry: &ManifestEntry) -> Result<VersionJson, Error> {
        const MAX_DEPTH: usize = 8;
        let mut current = self.version_for(entry).await?;
        for _ in 0..MAX_DEPTH {
            let Some(parent_id) = current.inherits_from.clone() else {
                return Ok(current);
            };
            // 父版本可能来自本地已安装的版本（loader 装完会把父版本 JSON 落盘），
            // 也可能只能从清单取；这里走清单，本地缓存由 install 流程负责。
            let manifest = self.manifest().await?;
            let parent = manifest
                .by_id(&parent_id)
                .ok_or_else(|| Error::Invalid(format!("unknown parent version: {parent_id}")))?;
            let parent_json = self.version_for(parent).await?;
            current = crate::version::merge(parent_json, current);
        }
        Err(Error::Invalid(format!(
            "version inheritance deeper than {MAX_DEPTH}: {}",
            entry.id
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
    fn unknown_version_is_none() {
        assert!(trimmed().by_id("99.99.99").is_none());
    }
}
