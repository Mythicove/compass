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
use crate::version::VersionJson;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    Fabric,
    Quilt,
    /// NeoForge（1.20.2+）。走 Maven + installer 的 processor pipeline，没有 meta 服务。
    Neoforge,
    /// Forge。同样走 Maven + installer，但**跨了三套安装格式**（实测，见 `install_from_installer`）：
    /// 1.13+ 有 processor（1.20.1 起带 `sides`，与 NeoForge 同构），1.12.2 及更早
    /// `processors` 是空数组、版本 JSON 用旧式 `minecraftArguments`。
    Forge,
}

impl LoaderKind {
    pub const ALL: [LoaderKind; 4] = [
        LoaderKind::Fabric,
        LoaderKind::Quilt,
        LoaderKind::Neoforge,
        LoaderKind::Forge,
    ];

    /// Fabric/Quilt 的 meta 服务根。**API 版本号是路径的一部分**（Fabric 用 v2、Quilt 用 v3），
    /// 不是我们能挑的，所以写死在这里而不是拼出来。NeoForge/Forge 没有 meta 服务 → None。
    pub fn meta_base(self) -> Option<&'static str> {
        match self {
            LoaderKind::Fabric => Some("https://meta.fabricmc.net/v2"),
            LoaderKind::Quilt => Some("https://meta.quiltmc.org/v3"),
            LoaderKind::Neoforge | LoaderKind::Forge => None,
        }
    }

    /// Maven 根（installer 与 maven-metadata.xml 都在这儿）。路径里的 groupId/artifactId
    /// 是各家自己定的，拼不出来。
    pub fn maven_base(self) -> Option<&'static str> {
        match self {
            LoaderKind::Neoforge => {
                Some("https://maven.neoforged.net/releases/net/neoforged/neoforge")
            }
            LoaderKind::Forge => Some("https://maven.minecraftforge.net/net/minecraftforge/forge"),
            _ => None,
        }
    }

    /// Maven 元数据列的是**全部**版本（NeoForge 1852 个、Forge 5094 个），要按游戏版本过滤，
    /// 而两家的编号约定不同：NeoForge 去掉开头的 `1.`（1.21.4 → `21.4.x`），
    /// Forge 直接 `<游戏版本>-`（1.21.4 → `1.21.4-54.1.22`，新的 `26.3-66.1.0` 也一样）。
    /// None = 这个约定推不出来（游戏版本不是 1.x 时的 NeoForge），调用方退回全量列表。
    pub fn game_prefix(self, game: &str) -> Option<String> {
        match self {
            LoaderKind::Forge => Some(format!("{game}-")),
            LoaderKind::Neoforge => game.strip_prefix("1.").map(|g| format!("{g}.")),
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
            LoaderKind::Forge => "forge",
        }
    }

    /// 版本列表怎么取：meta 服务给 JSON，Maven 给 `maven-metadata.xml`。
    pub fn uses_maven_metadata(self) -> bool {
        matches!(self, LoaderKind::Neoforge | LoaderKind::Forge)
    }

    /// installer jar 在 Maven 上的文件名前缀，也是缓存文件名前缀。
    /// 两家的 artifactId 恰好等于 `as_str()`，但那是巧合不是规则，所以单独一个方法。
    fn artifact(self) -> &'static str {
        self.as_str()
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

/// Maven 的 `maven-metadata.xml` 列的是**全部**版本（NeoForge 实测 1852 个、Forge 5094 个），
/// 所以要按游戏版本过滤，约定见 `LoaderKind::game_prefix`。
/// 过滤结果为空时退回完整列表——给用户一个能挑的长列表，好过一个空列表加一句猜不出原因的提示。
///
/// 文档顺序**不可信**：Forge 的元数据里 1.16.5 那段末尾是 `36.0.0`、`36.2.39` 在中间，
/// 所以排序不能靠"服务给的顺序"（见 `version_key`）。
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
    let Some(prefix) = kind.game_prefix(game) else {
        // 编号约定推不出来（游戏版本不是 1.x 的 NeoForge），别猜，直接给全量
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

/// 版本号排序键：主键 = 第一个 `-` 之前的数字段，次键 = 之后的数字段。
///
/// 次键是为 Forge 加的，而且必须是**全部**数字段而不是三段：
/// - `1.21.4-54.1.22` → 主 `[1,21,4]`、次 `[54,1,22]`
/// - `1.12.2-14.23.5.2864` → 次 `[14,23,5,2864]`，**四段**。只取三段的话
///   `…2860` 与 `…2864` 会并列，而 Maven 元数据的文档顺序不可信（实测 1.16.5
///   那段末尾是 `36.0.0`、`36.2.39` 在中间），并列就等于把"哪个最新"交给服务端。
///
/// 预发布标签不算版本号：`0.20.0-beta.9` 的次键是空（`beta` 不是数字），
/// 与 `0.20.0` 并列，先后仍由 `stable_rank` 决定——否则 beta 会排到正式版前面。
fn version_key(v: &str) -> (Vec<u32>, Vec<u32>) {
    let (main, suffix) = match v.split_once('-') {
        Some((m, s)) => (m, s),
        None => (v, ""),
    };
    (numeric_groups(main), numeric_groups(suffix))
}

/// 把 `14.23.5.2864` 这样的点分数字拆成段；只要有一段不是数字就整体当作"排不出来"
/// （返回空），交给 `sort_by` 的稳定性保持服务给的相对顺序。
fn numeric_groups(s: &str) -> Vec<u32> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split('.')
        .map(|p| p.parse::<u32>())
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default()
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
/// （如 `fabric-loader-0.20.1-1.21.4`、`neoforge-21.4.161`、`1.21.4-forge-54.1.22`），
/// 实例的 `version_id` 应当存它——那才是启动时真正用的版本。
pub async fn install(
    installer: &Installer,
    api: &ApiClient,
    kind: LoaderKind,
    game: &str,
    loader: &str,
    progress: Option<ProgressSink>,
) -> Result<InstalledVersion, Error> {
    if kind.uses_maven_metadata() {
        return install_from_installer(installer, api, kind, loader, progress).await;
    }
    let child = profile(api, kind, game, loader).await?;
    installer.install_profile(child, progress).await
}

/// NeoForge / Forge：下 installer → 读 profile → 装父版本 → 跑 client processors → 装子版本。
///
/// 顺序有硬约束：processor 的输入是 `{MINECRAFT_JAR}`（原版 client jar），
/// 所以父版本必须先装好；而 `{PATCHED}` 是 processor 的产物，
/// 所以子版本必须在 processor 之后装（否则库齐了但补丁 jar 还不存在）。
///
/// 游戏版本取自 installer 里的 `install_profile.json`（`minecraft` 字段），
/// 不用调用方给的：那才是这份 installer 真正对应的版本，传错了会在很后面才炸。
///
/// Forge 1.12.2 及更早**没有 processor**（`processors` 是空数组，实测
/// 1.12.2-14.23.5.2864 的 profile 里 `processors: []`、`spec: 0`），那一步整个跳过：
/// 它的版本 JSON 把 universal jar 当普通库列出来，靠 launchwrapper + `--tweakClass`
/// 启动，所以"一个 processor 都没跑"在那儿是正常状态，不是 profile 不符。
async fn install_from_installer(
    installer: &Installer,
    api: &ApiClient,
    kind: LoaderKind,
    loader_version: &str,
    progress: Option<ProgressSink>,
) -> Result<InstalledVersion, Error> {
    let paths = installer.paths().clone();
    let base = kind
        .maven_base()
        .ok_or_else(|| Error::Invalid(format!("{} has no maven repository", kind.as_str())))?;
    let jar_name = format!("{}-{loader_version}-installer.jar", kind.artifact());

    // 1. installer jar。Maven 会同时发 `<artifact>.sha1`，取来校验：
    //    这个 jar 里的 install_profile.json 决定我们**执行什么**，
    //    不校验就等于让链路上的任何一方改写它。
    let url = join_path(base, &[loader_version, &jar_name])?;
    let sha1 = api
        .get_text(&format!("{url}.sha1"))
        .await
        // 没有 .sha1 时退化成"无从校验"（空串），而不是直接失败：
        // 镜像不一定同步了校验和文件，那种情况下装还是能装的
        .ok()
        .map(|t| t.split_whitespace().next().unwrap_or_default().to_string())
        .filter(|s| s.len() == 40)
        .unwrap_or_default();
    let jar = paths.cache().join("installers").join(&jar_name);
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
            "failed to download the {} installer: {}",
            kind.as_str(),
            crate::net::format_failures(&summary.failed)
        )));
    }

    // 2. 读 profile 与子版本 JSON。v0（Forge ≤1.8.9）的版本 JSON 内联在 profile 里，
    //    新格式在 jar 的 `/version.json` 条目里——两种都由 read_installer 统一成同一个返回。
    let (profile, child) = crate::forge::read_installer(&jar)?;
    let game = profile.game_version().ok_or_else(|| {
        Error::Config(format!(
            "{} 的 install_profile.json 没说目标游戏版本（顶层 minecraft 与 install.minecraft 都是空的）",
            jar.display()
        ))
    })?;

    // 2b. installer 自带的产物。两种落点：
    //     - `maven/` 树（Forge 1.12.2 / 1.16.5 / 1.21.4 都有）
    //     - v0 的 universal jar 在 installer **根部**，且它那条库在 Maven 上是 404，
    //       只能从 installer 里取（实测见 extract_legacy_universal 的注释）
    //     都放在装子版本之前：下载器认"已就位"是靠 sha1，文件先到位它才会跳过那条任务。
    crate::forge::extract_installer_maven(&jar, &paths.libraries())?;
    crate::forge::extract_legacy_universal(&jar, &profile, &paths)?;

    // 3. 父版本先装好：processor 要拿它的 client jar 当输入
    installer.install(game, progress.clone()).await?;
    let mc_jar = paths.versions().join(game).join(format!("{game}.jar"));
    if !mc_jar.is_file() {
        return Err(Error::Config(format!(
            "client jar missing after installing {}: {}",
            game,
            mc_jar.display()
        )));
    }

    // 4./5. processor。Forge 1.12.2 及更早的 profile 里 `processors` 是空数组，
    //       这时连 Java 都不必探测——白找一个用不上的运行时，只会在没装 JDK 的
    //       机器上多报一句与真实问题无关的错。
    let client_steps = profile
        .processors
        .iter()
        .filter(|p| p.runs_on("client"))
        .count();
    if client_steps > 0 {
        // processor 用哪个 Java：按父版本的 majorVersion 探测（1.21.4 → 21）。
        // 用错大版本的话 binarypatcher 会以一句 ASM 版本报错收场，很难看出根因。
        let parent = crate::runner::read_installed_version(&paths, game)?;
        let (major, exact) = crate::runner::java_requirement(&parent);
        let java = crate::java::find(major, exact, Some(&paths.java()))
            .await
            .ok_or_else(|| {
                Error::Config(format!(
                    "no Java {major} runtime found ({} 的 processor 要用它跑)",
                    kind.as_str()
                ))
            })?;

        // 跑 client 侧的 processor（NeoForge 21.4 实测 6 个：MCP_DATA →
        // DOWNLOAD_MOJMAPS → MERGE_MAPPING → jarsplitter → AutoRenamingTool →
        // binarypatcher；Forge 1.20.1 是 6 个、1.21.4 是 3 个、1.16.5 是 4 个）
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
            // profile 明明列了 client 侧的 processor 却一个没跑 = sides 过滤错了，
            // 症状会推迟到启动时（缺补丁 jar），所以在这里就拦住
            return Err(Error::Config(format!(
                "install profile listed {client_steps} client-side processors but none ran"
            )));
        }
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
            LoaderKind::parse("Fabric").is_err(),
            "大小写敏感：取值就是 UI 传的字符串"
        );
        assert!(
            LoaderKind::parse("NeoForge").is_err(),
            "serde 的 lowercase 不是大小写不敏感"
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
        // NeoForge 与 Forge 都走 Maven：没有 meta 服务，两条路各自只认自己的入口
        for k in [LoaderKind::Neoforge, LoaderKind::Forge] {
            assert_eq!(k.meta_base(), None);
            assert!(k.uses_maven_metadata());
            assert!(k.maven_base().is_some());
        }
        assert!(!LoaderKind::Fabric.uses_maven_metadata());
        assert!(!LoaderKind::Quilt.uses_maven_metadata());
        assert_eq!(
            LoaderKind::Neoforge.maven_base(),
            Some("https://maven.neoforged.net/releases/net/neoforged/neoforge")
        );
        assert_eq!(
            LoaderKind::Forge.maven_base(),
            Some("https://maven.minecraftforge.net/net/minecraftforge/forge")
        );
        assert_eq!(LoaderKind::Fabric.maven_base(), None);
    }

    #[test]
    fn each_loader_filters_the_maven_list_by_its_own_naming_convention() {
        // 两家的编号约定不同，而且都不是我们能从游戏版本推出来的规则
        assert_eq!(
            LoaderKind::Neoforge.game_prefix("1.21.4").as_deref(),
            Some("21.4.")
        );
        assert_eq!(
            LoaderKind::Forge.game_prefix("1.21.4").as_deref(),
            Some("1.21.4-")
        );
        // Forge 的新编号（Minecraft 26.3）一样是 `<游戏版本>-`
        assert_eq!(
            LoaderKind::Forge.game_prefix("26.3").as_deref(),
            Some("26.3-")
        );
        // NeoForge 的约定推不出 1.x 以外的游戏版本 → None，调用方退回全量列表
        assert_eq!(LoaderKind::Neoforge.game_prefix("26.3"), None);
        // meta 服务已经按游戏版本给了列表，不需要前缀
        assert_eq!(LoaderKind::Fabric.game_prefix("1.21.4"), None);
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

        // Forge 同理，但版本号里带游戏版本前缀（实测这两个 URL 都可达）
        let fg_base = LoaderKind::Forge.maven_base().unwrap();
        assert_eq!(
            join_path(fg_base, &["maven-metadata.xml"]).unwrap(),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml"
        );
        assert_eq!(
            join_path(
                fg_base,
                &["1.21.4-54.1.22", "forge-1.21.4-54.1.22-installer.jar"]
            )
            .unwrap(),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.21.4-54.1.22/forge-1.21.4-54.1.22-installer.jar"
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
    fn sorts_forge_by_the_loader_part_when_the_game_part_ties() {
        // Forge 的版本号是 `<mc>-<forge>`：按游戏版本过滤后主键全等。
        // 这里的输入顺序**故意**照 maven.minecraftforge.net 的真实文档顺序摆
        // （36.0.0 在 36.2.39 之后），排完必须与文档顺序无关。
        let mut v = ["36.0.0", "36.0.8", "36.2.39", "36.1.0", "36.0.1"]
            .iter()
            .map(|s| LoaderVersion {
                version: format!("1.16.5-{s}"),
                stable: None,
            })
            .collect::<Vec<_>>();
        sort_versions(&mut v);
        let got: Vec<&str> = v.iter().map(|x| x.version.as_str()).collect();
        assert_eq!(
            got,
            vec![
                "1.16.5-36.2.39",
                "1.16.5-36.1.0",
                "1.16.5-36.0.8",
                "1.16.5-36.0.1",
                "1.16.5-36.0.0",
            ],
            "{got:?}"
        );

        // 不同游戏版本之间仍按游戏版本排（Forge 元数据里 5094 个版本混在一起）
        let mut mixed = ["1.16.5-36.2.39", "1.21.4-54.1.22", "26.3-66.1.0"]
            .iter()
            .map(|s| LoaderVersion {
                version: s.to_string(),
                stable: None,
            })
            .collect::<Vec<_>>();
        sort_versions(&mut mixed);
        assert_eq!(
            mixed.iter().map(|x| x.version.as_str()).collect::<Vec<_>>(),
            vec!["26.3-66.1.0", "1.21.4-54.1.22", "1.16.5-36.2.39"]
        );
    }

    #[test]
    fn sorts_legacy_forge_whose_loader_part_has_four_segments() {
        // 1.12.2 的 Forge 版本号是 `14.23.5.2864`——**四段**。只比三段的话
        // 2860 与 2864 并列，"最新"就由元数据的文档顺序决定，而那个顺序不可信。
        let mut v = [
            "1.12.2-14.23.5.2860",
            "1.12.2-14.23.5.2864",
            "1.12.2-14.23.4.2859",
            "1.12.2-14.23.5.2768",
        ]
        .iter()
        .map(|s| LoaderVersion {
            version: s.to_string(),
            stable: None,
        })
        .collect::<Vec<_>>();
        sort_versions(&mut v);
        assert_eq!(
            v.iter().map(|x| x.version.as_str()).collect::<Vec<_>>(),
            vec![
                "1.12.2-14.23.5.2864",
                "1.12.2-14.23.5.2860",
                "1.12.2-14.23.5.2768",
                "1.12.2-14.23.4.2859",
            ]
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
