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
    /// NeoForge（1.20.2+）。走 Maven + installer 的 processor pipeline，没有 meta 服务。
    Neoforge,
}

impl LoaderKind {
    pub const ALL: [LoaderKind; 3] = [LoaderKind::Fabric, LoaderKind::Quilt, LoaderKind::Neoforge];

    /// Fabric/Quilt 的 meta 服务根。**API 版本号是路径的一部分**（Fabric 用 v2、Quilt 用 v3），
    /// 不是我们能挑的，所以写死在这里而不是拼出来。NeoForge 没有 meta 服务 → None。
    pub fn meta_base(self) -> Option<&'static str> {
        match self {
            LoaderKind::Fabric => Some("https://meta.fabricmc.net/v2"),
            LoaderKind::Quilt => Some("https://meta.quiltmc.org/v3"),
            LoaderKind::Neoforge => None,
        }
    }

    /// NeoForge 的 Maven 根（installer 与 maven-metadata.xml 都在这儿）。
    pub fn maven_base(self) -> Option<&'static str> {
        match self {
            LoaderKind::Neoforge => {
                Some("https://maven.neoforged.net/releases/net/neoforged/neoforge")
            }
            _ => None,
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
            LoaderKind::Neoforge => "neoforge",
        }
    }

    /// 版本列表怎么取：meta 服务给 JSON，Maven 给 `maven-metadata.xml`。
    pub fn uses_maven_metadata(self) -> bool {
        matches!(self, LoaderKind::Neoforge)
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
    let mut out = if kind.uses_maven_metadata() {
        maven_versions(api, kind, game).await?
    } else {
        let base = kind
            .meta_base()
            .ok_or_else(|| Error::Invalid(format!("{} has no meta service", kind.as_str())))?;
        let url = join_path(base, &["versions", "loader", game])?;
        let entries: Vec<LoaderEntry> = api.get_json(&url).await?;
        entries
            .into_iter()
            .map(|e| LoaderVersion {
                version: e.loader.version,
                stable: e.loader.stable,
            })
            .collect()
    };
    out.retain(|v| !v.version.trim().is_empty());
    sort_versions(&mut out);
    Ok(out)
}

/// Maven 的 `maven-metadata.xml` 列的是**全部**版本（NeoForge 实测 1852 个），
/// 所以要按游戏版本过滤：NeoForge 的编号约定是"游戏版本去掉开头的 `1.` 再加构建号"
/// （1.21.4 → `21.4.x`）。这是它自己定的约定；万一哪天变了，过滤结果为空时
/// 退回完整列表——给用户一个能挑的长列表，好过一个空列表加一句猜不出原因的提示。
async fn maven_versions(
    api: &ApiClient,
    kind: LoaderKind,
    game: &str,
) -> Result<Vec<LoaderVersion>, Error> {
    let base = kind
        .maven_base()
        .ok_or_else(|| Error::Invalid(format!("{} has no maven repository", kind.as_str())))?;
    let url = join_path(base, &["maven-metadata.xml"])?;
    let xml = api.get_text(&url).await?;
    let all = parse_metadata_versions(&xml);
    if all.is_empty() {
        return Err(Error::Config(format!(
            "{url} carried no <version> entries (格式变了？)"
        )));
    }
    let Some(prefix) = game.strip_prefix("1.").map(|g| format!("{g}.")) else {
        // 游戏版本不是 1.x（未来的 2.x？），别猜编号规则，直接给全量
        return Ok(all
            .into_iter()
            .map(|version| LoaderVersion {
                version,
                stable: None,
            })
            .collect());
    };
    let matched: Vec<LoaderVersion> = all
        .iter()
        .filter(|v| v.starts_with(&prefix))
        .map(|version| LoaderVersion {
            version: version.clone(),
            // Maven 元数据没有"稳定"的概念；None = 来源没说（别显示成开发版）
            stable: None,
        })
        .collect();
    if matched.is_empty() {
        return Ok(all
            .into_iter()
            .map(|version| LoaderVersion {
                version,
                stable: None,
            })
            .collect());
    }
    Ok(matched)
}

/// 从 `maven-metadata.xml` 里取所有 `<version>` 值。
///
/// 不为此引 XML 解析器：这个文档只有一种形状，而扫描规则有真样本测试盯着。
/// 注意要匹配 `<version>` 整体而不是 `<version` 前缀——`<versioning>` 也以后者开头。
pub fn parse_metadata_versions(xml: &str) -> Vec<String> {
    const OPEN: &str = "<version>";
    const CLOSE: &str = "</version>";
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find(CLOSE) else { break };
        let v = after[..end].trim();
        if !v.is_empty() {
            out.push(v.to_string());
        }
        rest = &after[end + CLOSE.len()..];
    }
    out
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
///
/// 只对 Fabric/Quilt 有意义：NeoForge 的子版本 JSON 在 installer jar 里面，
/// 而且必须先跑完 processor pipeline 才谈得上安装（见 `install_neoforge`）。
pub async fn profile(
    api: &ApiClient,
    kind: LoaderKind,
    game: &str,
    loader: &str,
) -> Result<VersionJson, Error> {
    let base = kind
        .meta_base()
        .ok_or_else(|| Error::Invalid(format!("{} has no meta service", kind.as_str())))?;
    let url = join_path(
        base,
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

/// 装一个 loader 版本。返回的 `InstalledVersion.id` 是 profile 给的 id
/// （如 `fabric-loader-0.20.1-1.21.4`、`neoforge-21.4.161`），
/// 实例的 `version_id` 应当存它——那才是启动时真正用的版本。
pub async fn install(
    installer: &Installer,
    api: &ApiClient,
    kind: LoaderKind,
    game: &str,
    loader: &str,
    progress: Option<ProgressSink>,
) -> Result<InstalledVersion, Error> {
    if kind == LoaderKind::Neoforge {
        return install_neoforge(installer, api, loader, progress).await;
    }
    let child = profile(api, kind, game, loader).await?;
    installer.install_profile(child, progress).await
}

/// NeoForge：下 installer → 读 profile → 装父版本 → 跑 client processors → 装子版本。
///
/// 顺序有硬约束：processor 的输入是 `{MINECRAFT_JAR}`（原版 client jar），
/// 所以父版本必须先装好；而 `{PATCHED}` 是 processor 的产物，
/// 所以子版本必须在 processor 之后装（否则库齐了但补丁 jar 还不存在）。
///
/// 游戏版本取自 installer 里的 `install_profile.json`（`minecraft` 字段），
/// 不用调用方给的：那才是这份 installer 真正对应的版本，传错了会在很后面才炸。
async fn install_neoforge(
    installer: &Installer,
    api: &ApiClient,
    loader_version: &str,
    progress: Option<ProgressSink>,
) -> Result<InstalledVersion, Error> {
    let paths = installer.paths().clone();
    let base = LoaderKind::Neoforge
        .maven_base()
        .ok_or_else(|| Error::Invalid("neoforge has no maven repository".into()))?;

    // 1. installer jar。Maven 会同时发 `<artifact>.sha1`，取来校验：
    //    这个 jar 里的 install_profile.json 决定我们**执行什么**，
    //    不校验就等于让链路上的任何一方改写它。
    let url = join_path(
        base,
        &[
            loader_version,
            &format!("neoforge-{loader_version}-installer.jar"),
        ],
    )?;
    let sha1 = api
        .get_text(&format!("{url}.sha1"))
        .await
        // 没有 .sha1 时退化成"无从校验"（空串），而不是直接失败：
        // 镜像不一定同步了校验和文件，那种情况下装还是能装的
        .ok()
        .map(|t| t.split_whitespace().next().unwrap_or_default().to_string())
        .filter(|s| s.len() == 40)
        .unwrap_or_default();
    let jar = paths
        .cache()
        .join("installers")
        .join(format!("neoforge-{loader_version}-installer.jar"));
    let summary = installer
        .downloader()
        .download_all(
            vec![crate::net::FileTask {
                url: url.clone(),
                dest: jar.clone(),
                sha1,
                size: 0,
            }],
            progress.clone(),
        )
        .await;
    if !summary.failed.is_empty() {
        return Err(Error::Config(format!(
            "failed to download the NeoForge installer: {}",
            crate::net::format_failures(&summary.failed)
        )));
    }

    // 2. 读 profile 与子版本 JSON（都在 jar 里）
    let (profile, child) = crate::forge::read_installer(&jar)?;

    // 3. 父版本先装好：processor 要拿它的 client jar 当输入
    installer
        .install(&profile.minecraft, progress.clone())
        .await?;
    let mc_jar = paths
        .versions()
        .join(&profile.minecraft)
        .join(format!("{}.jar", profile.minecraft));
    if !mc_jar.is_file() {
        return Err(Error::Config(format!(
            "client jar missing after installing {}: {}",
            profile.minecraft,
            mc_jar.display()
        )));
    }

    // 4. processor 用哪个 Java：按父版本的 majorVersion 探测（1.21.4 → 21）。
    //    用错大版本的话 binarypatcher 会以一句 ASM 版本报错收场，很难看出根因。
    let parent = crate::runner::read_installed_version(&paths, &profile.minecraft)?;
    let (major, exact) = crate::runner::java_requirement(&parent);
    let java = crate::java::find(major, exact, Some(&paths.java()))
        .await
        .ok_or_else(|| {
            Error::Config(format!(
                "no Java {major} runtime found (NeoForge 的 processor 要用它跑)"
            ))
        })?;

    // 5. 跑 client 侧的 processor（实测 6 个：MCP_DATA → DOWNLOAD_MOJMAPS →
    //    MERGE_MAPPING → jarsplitter → AutoRenamingTool → binarypatcher）
    let ran = crate::forge::run_client_processors(
        &paths,
        installer.downloader(),
        &jar,
        &profile,
        &java.path,
        &mc_jar,
        progress.clone(),
    )
    .await?;
    if ran == 0 {
        return Err(Error::Config(
            "install profile listed no client-side processors (profile 与预期不符)".into(),
        ));
    }

    // 6. 装子版本：下载它自己的库 + 落盘合并后的 JSON
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
            "Forge 还没接（W9 后半段）"
        );
        assert!(
            LoaderKind::parse("Fabric").is_err(),
            "大小写敏感：取值就是 UI 传的字符串"
        );
        // base 里的 API 版本号是各家自己定的，不是我们能推的
        assert_eq!(
            LoaderKind::Fabric.meta_base(),
            Some("https://meta.fabricmc.net/v2")
        );
        assert_eq!(
            LoaderKind::Quilt.meta_base(),
            Some("https://meta.quiltmc.org/v3")
        );
        // NeoForge 走 Maven：没有 meta 服务，两条路各自只认自己的入口
        assert_eq!(LoaderKind::Neoforge.meta_base(), None);
        assert!(LoaderKind::Neoforge.uses_maven_metadata());
        assert!(!LoaderKind::Fabric.uses_maven_metadata());
        assert!(!LoaderKind::Quilt.uses_maven_metadata());
        assert_eq!(
            LoaderKind::Neoforge.maven_base(),
            Some("https://maven.neoforged.net/releases/net/neoforged/neoforge")
        );
        assert_eq!(LoaderKind::Fabric.maven_base(), None);
    }

    #[test]
    fn parses_maven_metadata_versions() {
        // 真样本的骨架（neoforge 的 maven-metadata.xml 就是这个形状，1852 个 version）
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<metadata>
  <groupId>net.neoforged</groupId>
  <artifactId>neoforge</artifactId>
  <versioning>
    <latest>21.4.161</latest>
    <release>21.4.161</release>
    <versions>
      <version>20.4.249</version>
      <version>21.4.158</version>
      <version>21.4.161</version>
    </versions>
    <lastUpdated>20261028000000</lastUpdated>
  </versioning>
</metadata>"#;
        let got = parse_metadata_versions(xml);
        // `<versioning>`/`<latest>`/`<release>` 都不能被当成版本条目
        assert_eq!(got, vec!["20.4.249", "21.4.158", "21.4.161"], "{got:?}");
        assert!(parse_metadata_versions("<metadata/>").is_empty());
        // 不闭合的标签不该 panic，也不该给出半个版本号
        assert!(parse_metadata_versions("<version>1.2.3").is_empty());
    }

    #[test]
    fn neoforge_versions_are_filtered_by_the_game_version_convention() {
        // NeoForge 的编号是"游戏版本去掉开头的 1. 再加构建号"：1.21.4 → 21.4.x
        let all = ["20.4.249", "21.4.158", "21.4.161", "21.5.1"];
        let prefix = "1.21.4".strip_prefix("1.").map(|g| format!("{g}."));
        assert_eq!(prefix.as_deref(), Some("21.4."));
        let matched: Vec<&str> = all
            .iter()
            .filter(|v| v.starts_with(prefix.as_deref().unwrap()))
            .copied()
            .collect();
        assert_eq!(matched, vec!["21.4.158", "21.4.161"], "{matched:?}");
        // 21.5.1 属于 1.21.5，不该混进 1.21.4 的列表
        assert!(!matched.contains(&"21.5.1"));
    }

    #[test]
    fn loader_urls_are_built_from_escaped_segments() {
        // 游戏版本与 loader 版本都来自 UI/服务端，直接内插会让 `../`、`?`、`#` 改写请求目标
        let fabric_base = LoaderKind::Fabric.meta_base().unwrap();
        let fabric = join_path(
            fabric_base,
            &["versions", "loader", "1.21.4", "0.16.9", "profile", "json"],
        )
        .unwrap();
        assert_eq!(
            fabric,
            "https://meta.fabricmc.net/v2/versions/loader/1.21.4/0.16.9/profile/json"
        );
        let quilt = join_path(
            LoaderKind::Quilt.meta_base().unwrap(),
            &["versions", "loader", "1.21.4"],
        )
        .unwrap();
        assert_eq!(quilt, "https://meta.quiltmc.org/v3/versions/loader/1.21.4");

        // NeoForge 的 installer 与 maven-metadata.xml 都在 Maven 上（实测可达）
        let nf_base = LoaderKind::Neoforge.maven_base().unwrap();
        assert_eq!(
            join_path(nf_base, &["maven-metadata.xml"]).unwrap(),
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml"
        );
        assert_eq!(
            join_path(nf_base, &["21.4.161", "neoforge-21.4.161-installer.jar"]).unwrap(),
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/21.4.161/neoforge-21.4.161-installer.jar"
        );

        let evil = join_path(fabric_base, &["versions", "loader", "../../x"]).unwrap();
        assert!(
            evil.contains("%2F") || !evil.contains("/x"),
            "分隔符必须被编码，不能真的跳到别的路径：{evil}"
        );
        // 版本号位置上的穿越同样不能改写请求目标（NeoForge 的 installer URL 就是这么拼的）
        let evil_nf = join_path(nf_base, &["../../evil", "x.jar"]).unwrap();
        assert!(
            evil_nf.contains("%2F") || !evil_nf.contains("/evil/x.jar"),
            "{evil_nf}"
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
