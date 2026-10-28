//! Minecraft 版本 JSON：解析、平台规则求值、库解析、启动参数构造、版本继承合并。
//!
//! 这个模块的正确性是整个启动器的地基，所以契约测试直接对着**真实版本 JSON** 跑
//! （`tests/fixtures/`：1.7.10 / 1.12.2 / 1.16.5 / 1.20.1 / 1.21.4 覆盖 legacy
//! `minecraftArguments` 与 modern `arguments` 两套格式，外加一个真实的 Fabric
//! 继承型版本）。错一个占位符或一条规则的表现是"游戏能启动但皮肤不出、存档找不到、
//! natives 加载失败"，比直接崩更难查。
//!
//! 几个从真实样本里挖出来、不看就会踩的坑：
//! - 现代版本（1.19+）的原生库是 **4 段 Maven 坐标**（`org.lwjgl:lwjgl:3.3.3:natives-windows`），
//!   靠 `rules.os.name` 选中，**没有** `natives`/`extract` 字段，且要进 classpath
//!   （LWJGL 3 自己从 classpath 的 jar 里释放 natives）；老版本才有 `natives` + `extract`，
//!   那类 jar 要解压到 natives 目录且**不进** classpath。
//! - `downloads.artifact` 可以整个缺失（只有 classifiers 的原生库），此时非本平台就跳过整条。
//! - Fabric/Quilt 生成的库**没有 `downloads` 块**：`sha1`/`size` 直接放在库对象上，
//!   `url` 是 Maven 仓库基址，路径要按坐标自己拼。
//! - 1.21.4 的 macOS/Windows arm64 natives 只有 `os.name` 规则、**没有 arch 规则**：
//!   同一平台会有多条命中，全部放行即可（LWJGL 运行时按架构挑），不要试图自己筛。
//! - 规则语义是"**最后一条命中的规则决定**"，且"有规则但都不命中"= 拒绝。

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// 版本 JSON 里 `features` 规则的求值上下文（`is_demo_user`、`has_custom_resolution`、
/// `has_quick_plays_support` 等）。用 map 而不是结构体：Mojang 会新增特性名，
/// 结构体会让未知特性静默变 false 之外的第三种状态（编译不过）。
pub type Features = HashMap<String, bool>;

// ---------------------------------------------------------------------------
// 目标平台
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsName {
    Windows,
    Osx,
    Linux,
}

impl OsName {
    /// 版本 JSON 里 macOS 写作 `osx`（不是 `macos`）。
    pub fn as_str(self) -> &'static str {
        match self {
            OsName::Windows => "windows",
            OsName::Osx => "osx",
            OsName::Linux => "linux",
        }
    }

    pub fn current() -> Self {
        match std::env::consts::OS {
            "windows" => OsName::Windows,
            "macos" => OsName::Osx,
            _ => OsName::Linux,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86,
    X64,
    Arm64,
}

impl Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X64 => "x64",
            Arch::Arm64 => "arm64",
        }
    }

    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86" => Arch::X86,
            "aarch64" | "arm64" => Arch::Arm64,
            _ => Arch::X64,
        }
    }

    /// 老版本 `natives-windows-${arch}` 的历史约定：32 位写 32，其余写 64。
    pub fn legacy_suffix(self) -> &'static str {
        match self {
            Arch::X86 => "32",
            _ => "64",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OsInfo {
    pub name: OsName,
    /// 操作系统版本串（macOS 的 `^10\.` 之类规则要用）。
    pub version: String,
    pub arch: Arch,
}

impl OsInfo {
    pub fn current() -> Self {
        Self {
            name: OsName::current(),
            version: os_info::get().version().to_string(),
            arch: Arch::current(),
        }
    }

    /// 显式构造：规则测试必须在任一 CI 平台上覆盖三个系统的分支。
    pub fn new(name: OsName, version: impl Into<String>, arch: Arch) -> Self {
        Self {
            name,
            version: version.into(),
            arch,
        }
    }
}

// ---------------------------------------------------------------------------
// 规则
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    Allow,
    Disallow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
    /// 正则，匹配 `OsInfo::version`。
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub action: RuleAction,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: Option<HashMap<String, bool>>,
}

impl Rule {
    fn matches(&self, os: &OsInfo, features: &Features) -> bool {
        if let Some(os_rule) = &self.os {
            if os_rule
                .name
                .as_deref()
                .is_some_and(|n| n != os.name.as_str())
            {
                return false;
            }
            if os_rule
                .arch
                .as_deref()
                .is_some_and(|a| a != os.arch.as_str())
            {
                return false;
            }
            // os.version 是正则；非法正则按不命中处理（一条坏规则不该 panic 掉启动流程）
            if let Some(pattern) = &os_rule.version {
                let matched = Regex::new(pattern).is_ok_and(|re| re.is_match(&os.version));
                if !matched {
                    return false;
                }
            }
        }
        if let Some(wanted) = &self.features {
            for (key, value) in wanted {
                if features.get(key).copied().unwrap_or(false) != *value {
                    return false;
                }
            }
        }
        true
    }
}

/// 规则求值：**最后一条命中的规则决定结果**；有规则但一条都不命中 = 拒绝；无规则 = 允许。
pub fn rules_allow(rules: Option<&[Rule]>, os: &OsInfo, features: &Features) -> bool {
    let Some(rules) = rules else {
        return true;
    };
    let mut allowed = false;
    for rule in rules {
        if rule.matches(os, features) {
            allowed = rule.action == RuleAction::Allow;
        }
    }
    allowed
}

// ---------------------------------------------------------------------------
// 版本 JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    #[serde(default)]
    pub path: Option<String>,
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<Download>,
    #[serde(default)]
    pub classifiers: Option<HashMap<String, Download>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extract {
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    /// Maven 坐标，3 段或 4 段（第 4 段是 classifier）。
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    /// Fabric/Quilt 风格：没有 `downloads` 块，`url` 是 Maven 仓库基址，
    /// `sha1`/`size` 直接放在库对象上。
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub rules: Option<Vec<Rule>>,
    #[serde(default)]
    pub natives: Option<HashMap<String, String>>,
    #[serde(default)]
    pub extract: Option<Extract>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgValue {
    One(String),
    Many(Vec<String>),
}

impl ArgValue {
    pub fn items(&self) -> &[String] {
        match self {
            ArgValue::One(s) => std::slice::from_ref(s),
            ArgValue::Many(v) => v,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgItem {
    Plain(String),
    Conditional {
        #[serde(default)]
        rules: Vec<Rule>,
        value: ArgValue,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<ArgItem>,
    #[serde(default)]
    pub jvm: Option<Vec<ArgItem>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndex {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub total_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Downloads {
    #[serde(default)]
    pub client: Option<Download>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    /// 自动下载用的运行时名（`jre-legacy` / `java-runtime-gamma` / `-delta` …）。
    pub component: String,
    /// 真正决定"要哪个大版本"的是这个数，component 只用于下载。
    pub major_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub argument: String,
    pub file: LoggingFile,
    #[serde(rename = "type", default)]
    pub kind: String,
}

/// 日志配置文件：比普通 Download 多一个 id（落盘文件名）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingFile {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Logging {
    #[serde(default)]
    pub client: Option<LoggingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub main_class: Option<String>,
    #[serde(default)]
    pub inherits_from: Option<String>,
    /// 继承型版本用它指定"实际的 jar 属于哪个版本"；缺省用 `inherits_from`。
    #[serde(default)]
    pub jar: Option<String>,
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    #[serde(default)]
    pub asset_index: Option<AssetIndex>,
    #[serde(default)]
    pub assets: Option<String>,
    #[serde(default)]
    pub downloads: Option<Downloads>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    #[serde(default)]
    pub logging: Option<Logging>,
    #[serde(default)]
    pub minimum_launcher_version: Option<u32>,
}

impl VersionJson {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// 资源索引 id：优先 `assets`，回落 `assetIndex.id`（两者绝大多数情况相同）。
    pub fn asset_index_id(&self) -> Option<&str> {
        self.assets
            .as_deref()
            .or(self.asset_index.as_ref().map(|a| a.id.as_str()))
    }

    /// 实际提供 client jar 的版本 id（继承型版本要取父版本）。
    pub fn jar_version(&self) -> &str {
        self.jar
            .as_deref()
            .or(self.inherits_from.as_deref())
            .unwrap_or(&self.id)
    }
}

// ---------------------------------------------------------------------------
// 库解析
// ---------------------------------------------------------------------------

/// 官方默认 Maven 仓库；库自带 `url` 时以它为准。
pub const DEFAULT_LIBRARY_REPO: &str = "https://libraries.minecraft.net/";

#[derive(Debug, Clone, PartialEq)]
pub enum LibraryKind {
    /// 进 classpath。
    Classpath,
    /// 解压到 natives 目录（不进 classpath）；`exclude` 是解压时要跳过的路径前缀。
    Native { exclude: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLibrary {
    pub kind: LibraryKind,
    /// 相对 libraries 根目录的路径（Maven 布局），也是本地缓存的落点。
    pub path: String,
    pub url: String,
    /// 空串表示来源没给校验和（Fabric 的老格式偶尔如此）：下载后无从校验，只能记录。
    pub sha1: String,
    pub size: u64,
}

/// Maven 坐标 → 仓库相对路径。3 段 `g:a:v`，4 段 `g:a:v:classifier`。
pub fn maven_path(name: &str) -> Option<String> {
    let p: Vec<&str> = name.split(':').collect();
    match p.len() {
        3 => Some(format!(
            "{}/{}/{}/{}-{}.jar",
            p[0].replace('.', "/"),
            p[1],
            p[2],
            p[1],
            p[2]
        )),
        4 => Some(format!(
            "{}/{}/{}/{}-{}-{}.jar",
            p[0].replace('.', "/"),
            p[1],
            p[2],
            p[1],
            p[2],
            p[3]
        )),
        _ => None,
    }
}

fn join_url(base: &str, path: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

/// 老版本的 `natives` 映射：`{"windows": "natives-windows-${arch}"}`。
fn native_classifier(lib: &Library, os: &OsInfo) -> Option<String> {
    let key = lib.natives.as_ref()?.get(os.name.as_str())?;
    Some(key.replace("${arch}", os.arch.legacy_suffix()))
}

/// 把版本 JSON 的库列表解析成"这台机器上要下载/使用的文件"。
///
/// 选择顺序：先看规则是否放行；再决定用哪个下载项——
/// 有 `natives` 命中就用对应 classifier（解压型），否则用 `downloads.artifact`（classpath 型），
/// 两者都没有就按坐标 + 仓库基址拼（Fabric/Quilt 风格）。
pub fn resolve_libraries(
    v: &VersionJson,
    os: &OsInfo,
    features: &Features,
) -> Vec<ResolvedLibrary> {
    let mut out = Vec::new();
    for lib in &v.libraries {
        if !rules_allow(lib.rules.as_deref(), os, features) {
            continue;
        }
        let exclude = lib
            .extract
            .as_ref()
            .map(|e| e.exclude.clone())
            .unwrap_or_default();

        // 1) 老式 natives：按平台取 classifier
        if let Some(classifier) = native_classifier(lib, os) {
            let found = lib
                .downloads
                .as_ref()
                .and_then(|d| d.classifiers.as_ref())
                .and_then(|c| c.get(&classifier))
                .cloned();
            match found {
                Some(dl) => {
                    let path = dl
                        .path
                        .clone()
                        .or_else(|| maven_path(&lib.name))
                        .unwrap_or_default();
                    out.push(ResolvedLibrary {
                        kind: LibraryKind::Native { exclude },
                        path,
                        url: dl.url,
                        sha1: dl.sha1,
                        size: dl.size,
                    });
                    continue;
                }
                // 本平台没有对应 classifier：整条跳过（例如老 LWJGL 没有 arm64 包）
                None => continue,
            }
        }

        // 2) 现代：downloads.artifact（含 4 段坐标的 natives-*，它们进 classpath）
        if let Some(artifact) = lib.downloads.as_ref().and_then(|d| d.artifact.clone()) {
            let path = artifact
                .path
                .clone()
                .or_else(|| maven_path(&lib.name))
                .unwrap_or_default();
            out.push(ResolvedLibrary {
                kind: LibraryKind::Classpath,
                path,
                url: artifact.url,
                sha1: artifact.sha1,
                size: artifact.size,
            });
            continue;
        }

        // 3) Fabric/Quilt 风格：无 downloads 块，按坐标 + 仓库基址拼
        let Some(path) = maven_path(&lib.name) else {
            continue;
        };
        let base = lib
            .url
            .as_deref()
            .filter(|u| !u.is_empty())
            .unwrap_or(DEFAULT_LIBRARY_REPO);
        out.push(ResolvedLibrary {
            kind: LibraryKind::Classpath,
            path: path.clone(),
            url: join_url(base, &path),
            sha1: lib.sha1.clone().unwrap_or_default(),
            size: lib.size.unwrap_or(0),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// 启动参数
// ---------------------------------------------------------------------------

/// 取版本的 (major, minor, patch) 数字前缀；缺失的段按 0 计，非数字开头返回 None。
///
/// **全站只有这一份版本比较实现**：assets 的 virtual 布局判定、Log4Shell 缓解、
/// Java 版本选择都要"这个版本比那个版本老吗"，各写一份迟早对不上。
pub fn version_parts(version: &str) -> Option<(u32, u32, u32)> {
    let take = |s: &str| -> Option<u32> { s.split(['-', '_']).next()?.parse().ok() };
    let mut parts = version.split('.');
    let major = take(parts.next()?)?;
    let minor = parts.next().and_then(take).unwrap_or(0);
    let patch = parts.next().and_then(take).unwrap_or(0);
    Some((major, minor, patch))
}

/// 该版本是否受 Log4Shell（CVE-2021-44228）影响：1.7 到 **1.18.1** 都带着受影响的 log4j2，
/// 1.18.2 起 Mojang 换成了修好的版本。缓解方式是加 `-Dlog4j2.formatMsgNoLookups=true`。
pub fn needs_log4shell_mitigation(version: &str) -> bool {
    let Some((major, minor, patch)) = version_parts(version) else {
        // 解析不出来（快照等）：1.18.2 之后的快照都已修好，更老的不在支持范围内
        return false;
    };
    if major != 1 {
        return false;
    }
    match minor {
        7..=17 => true,
        18 => patch < 2,
        _ => false,
    }
}

/// 替换 `${key}`；未提供的键原样保留（比默默换成空串好诊断得多）。
pub fn substitute(token: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(token.len());
    let mut rest = token;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        let key = &after[..end];
        out.push_str(&rest[..start]);
        match vars.get(key) {
            Some(value) => out.push_str(value),
            // 未知占位符原样保留：日志里看得见，不至于变成一段无声的空参数
            None => {
                out.push_str("${");
                out.push_str(key);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn expand(
    items: &[ArgItem],
    os: &OsInfo,
    features: &Features,
    vars: &HashMap<String, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        match item {
            ArgItem::Plain(s) => out.push(substitute(s, vars)),
            ArgItem::Conditional { rules, value } => {
                if rules_allow(Some(rules), os, features) {
                    for v in value.items() {
                        out.push(substitute(v, vars));
                    }
                }
            }
        }
    }
    out
}

/// 没有 `arguments.jvm` 的老版本用这组默认 JVM 参数（与官方启动器一致）。
const LEGACY_JVM_ARGS: &[&str] = &[
    "-Djava.library.path=${natives_directory}",
    "-Dminecraft.launcher.brand=${launcher_name}",
    "-Dminecraft.launcher.version=${launcher_version}",
    "-cp",
    "${classpath}",
];

pub fn jvm_arguments(
    v: &VersionJson,
    os: &OsInfo,
    features: &Features,
    vars: &HashMap<String, String>,
) -> Vec<String> {
    if let Some(items) = v.arguments.as_ref().and_then(|a| a.jvm.as_ref()) {
        return expand(items, os, features, vars);
    }
    LEGACY_JVM_ARGS
        .iter()
        .map(|a| substitute(a, vars))
        .collect()
}

pub fn game_arguments(
    v: &VersionJson,
    os: &OsInfo,
    features: &Features,
    vars: &HashMap<String, String>,
) -> Vec<String> {
    if let Some(args) = &v.arguments {
        return expand(&args.game, os, features, vars);
    }
    // legacy：minecraftArguments 是一整串以空格分隔的参数
    v.minecraft_arguments
        .as_deref()
        .unwrap_or("")
        .split_whitespace()
        .map(|a| substitute(a, vars))
        .collect()
}

// ---------------------------------------------------------------------------
// 版本继承合并（Fabric/Quilt/Forge/NeoForge 都是"子版本 + inheritsFrom"）
// ---------------------------------------------------------------------------

/// 合并父子版本。
///
/// - 身份（id/mainClass/type）来自子版本；
/// - 库是"子在前 + 父在后"（子在前的先进 classpath，从而覆盖父的同名类）；
/// - `game` 参数子先父后（与 MultiMC/HMCL 的既有约定一致）；
/// - `jvm` 参数是**父 + 子拼接**，不是子覆盖父：真实的 Fabric profile 里子版本的 jvm
///   只有一条 `-DFabricMcEmu= net.minecraft.client.main.Main`，若用它替换父版本，
///   启动命令里就没有 `-Djava.library.path` 也没有 `-cp`，游戏根本起不来。
///   JVM 选项之间的顺序无关紧要（只要都在主类之前），故拼接是安全的。
/// - 资源索引/下载/日志/Java 版本子缺则取父；
/// - 库按 Maven 身份去重（`library_key`），子版本赢。
pub fn merge(parent: VersionJson, child: VersionJson) -> VersionJson {
    let arguments = match (child.arguments, parent.arguments) {
        (Some(c), Some(p)) => Some(Arguments {
            game: c.game.into_iter().chain(p.game).collect(),
            jvm: match (p.jvm, c.jvm) {
                (Some(pj), Some(cj)) => Some(pj.into_iter().chain(cj).collect()),
                (Some(pj), None) => Some(pj),
                (None, cj) => cj,
            },
        }),
        (Some(c), None) => Some(c),
        (None, p) => p,
    };
    VersionJson {
        id: child.id,
        kind: if child.kind.is_empty() {
            parent.kind
        } else {
            child.kind
        },
        main_class: child.main_class.or(parent.main_class),
        jar: child.jar,
        inherits_from: child.inherits_from,
        minecraft_arguments: child.minecraft_arguments.or(parent.minecraft_arguments),
        arguments,
        asset_index: child.asset_index.or(parent.asset_index),
        assets: child.assets.or(parent.assets),
        downloads: child.downloads.or(parent.downloads),
        libraries: dedupe_libraries(
            child
                .libraries
                .into_iter()
                .chain(parent.libraries)
                .collect(),
        ),
        java_version: child.java_version.or(parent.java_version),
        logging: child.logging.or(parent.logging),
        minimum_launcher_version: match (
            child.minimum_launcher_version,
            parent.minimum_launcher_version,
        ) {
            (Some(c), Some(p)) => Some(c.max(p)),
            (c, p) => c.or(p),
        },
    }
}

/// 这个版本的游戏 jar 是否**由 loader 自己提供**（而不是由启动器放进 classpath）。
///
/// 判据是游戏参数里有没有 `--launchTarget`——那是 ModLauncher 的入口参数，
/// Forge 1.17+ 与 NeoForge 都走它，而它们的 `fml_loader` 会**自己按坐标**去
/// libraries 目录找打过补丁的 client jar（启动日志里那句
/// `Found mod file ... [locator: production client provider +net.neoforged:neoforge:21.4.161:client]`
/// 就是它干的）。
///
/// 这时启动器再往 classpath 里塞任何游戏 jar 都会撞上**自动模块重名**，实测三种组合：
/// - 加原版 `1.21.4.jar` → `Modules _1._21._4 and minecraft export package net.minecraft.…`
/// - 加补丁 `neoforge-21.4.161-client.jar` → `Module mixin_synthetic reads more than one
///   module named neoforge`（它与 `neoforge-…-universal.jar` 的自动模块名都是 `neoforge`；
///   同理 `client-…-srg.jar` 与 `client-…-extra.jar` 都叫 `client`）
/// - **只放库、一个游戏 jar 都不加** → NeoForge 一路跑到 `Launching target 'neoforgeclient'`、
///   `Setting user:` 与 `Backend library: LWJGL version 3.3.3+5`
///
/// 用数据判而不是按 loader 名字/版本表判：Fabric 与 Quilt 走 KnotClient，
/// 必须有原版 jar 在 classpath 上；Forge 1.12 及更早用 `--tweakClass`，同样要原版 jar。
/// 四种情况这一个判据都对。
pub fn loader_provides_game_jar(v: &VersionJson) -> bool {
    const NEEDLE: &str = "--launchTarget";
    if let Some(args) = &v.arguments {
        return args.game.iter().any(|item| match item {
            ArgItem::Plain(s) => s == NEEDLE,
            ArgItem::Conditional { value, .. } => value.items().iter().any(|s| s == NEEDLE),
        });
    }
    v.minecraft_arguments
        .as_deref()
        .is_some_and(|s| s.split_whitespace().any(|a| a == NEEDLE))
}

/// 库去重的键：`group:artifact[:classifier[@ext]]`——**不含版本号**。
///
/// 版本号不进键：父子给了同一个逻辑库的不同版本时，子版本赢（合并时子版本的库
/// 排在前面，而 classpath 上先出现的先被加载）。
/// classifier 必须进键：`org.lwjgl:lwjgl:3.3.3` 与 `org.lwjgl:lwjgl:3.3.3:natives-windows`
/// 是两个不同的文件，都要留在 classpath 上。
pub fn library_key(name: &str) -> String {
    let parts: Vec<&str> = name.split(':').collect();
    if parts.len() < 3 {
        // 段数不足的畸形坐标原样返回：数据来自服务端，不该在这里 panic
        return name.to_string();
    }
    // parts[2] 是版本，跳过；后面的（classifier、@ext）都进键
    let mut key = format!("{}:{}", parts[0], parts[1]);
    for p in &parts[3..] {
        key.push(':');
        key.push_str(p);
    }
    key
}

/// 按 `library_key` 去重，保留**第一次出现**的那条。
///
/// 不是可选的清理：原版 1.21.4 带 `org.ow2.asm:asm:9.6`，而 NeoForge 带 9.8
/// 并且把 9.8 放进了模块路径（`-p`）。两份 asm 同时出现在 classpath 上时，
/// BootstrapLauncher 会直接抛 `Module named org.objectweb.asm was already on the
/// JVMs module path ... but class-path contains it at ...asm-9.6.jar`——
/// 这是拿官方 installer 的产物真跑一次启动时看到的报错，不是推测。
/// Fabric 同样会重复带 asm（9.10.1 对 9.6），只是它没有模块路径检查，所以一直没炸。
fn dedupe_libraries(libs: Vec<Library>) -> Vec<Library> {
    let mut seen = std::collections::HashSet::new();
    libs.into_iter()
        .filter(|l| seen.insert(library_key(&l.name)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 真实版本 JSON（从 piston-meta 取回后原样入库）：解析与推导的正确性只能对着真样本验，
    // 手写假样本会把"我以为的格式"当成契约。
    fn v(name: &str, json: &str) -> VersionJson {
        VersionJson::parse(json).unwrap_or_else(|e| panic!("parse {name}: {e}"))
    }
    fn v1214() -> VersionJson {
        v("1.21.4", include_str!("../tests/fixtures/1.21.4.json"))
    }
    fn v1201() -> VersionJson {
        v("1.20.1", include_str!("../tests/fixtures/1.20.1.json"))
    }
    fn v1165() -> VersionJson {
        v("1.16.5", include_str!("../tests/fixtures/1.16.5.json"))
    }
    fn v1122() -> VersionJson {
        v("1.12.2", include_str!("../tests/fixtures/1.12.2.json"))
    }
    fn v1710() -> VersionJson {
        v("1.7.10", include_str!("../tests/fixtures/1.7.10.json"))
    }
    fn fabric() -> VersionJson {
        v(
            "fabric",
            include_str!("../tests/fixtures/fabric-1.21.4.json"),
        )
    }
    /// 官方 NeoForge installer 写出的那份 version.json，原样收进来（未裁剪）。
    fn neoforge() -> VersionJson {
        v(
            "neoforge",
            include_str!("../tests/fixtures/neoforge-21.4.161.json"),
        )
    }

    fn win() -> OsInfo {
        OsInfo::new(OsName::Windows, "10.0.19045", Arch::X64)
    }
    fn win_x86() -> OsInfo {
        OsInfo::new(OsName::Windows, "10.0.19045", Arch::X86)
    }
    fn mac_arm() -> OsInfo {
        OsInfo::new(OsName::Osx, "14.5", Arch::Arm64)
    }
    fn mac_old() -> OsInfo {
        OsInfo::new(OsName::Osx, "10.13.6", Arch::X64)
    }
    fn linux() -> OsInfo {
        OsInfo::new(OsName::Linux, "6.8.0", Arch::X64)
    }
    fn no_features() -> Features {
        Features::new()
    }
    fn vars() -> HashMap<String, String> {
        HashMap::from([
            ("auth_player_name".to_string(), "Alice".to_string()),
            (
                "auth_uuid".to_string(),
                "7e01ed00f3453fbcb4437bac57a28a65".to_string(),
            ),
            ("auth_access_token".to_string(), "tok".to_string()),
            ("user_type".to_string(), "msa".to_string()),
            ("user_properties".to_string(), "{}".to_string()),
            ("version_name".to_string(), "1.21.4".to_string()),
            ("version_type".to_string(), "release".to_string()),
            ("game_directory".to_string(), "/game".to_string()),
            ("assets_root".to_string(), "/assets".to_string()),
            ("assets_index_name".to_string(), "19".to_string()),
            ("natives_directory".to_string(), "/natives".to_string()),
            ("classpath".to_string(), "/cp/a.jar;/cp/b.jar".to_string()),
            ("launcher_name".to_string(), "MythicCompass".to_string()),
            ("launcher_version".to_string(), "0.2.0".to_string()),
            ("clientid".to_string(), "cid".to_string()),
            ("auth_xuid".to_string(), "xuid".to_string()),
            ("path".to_string(), "/logging/client.xml".to_string()),
        ])
    }

    fn has(args: &[String], needle: &str) -> bool {
        args.iter().any(|a| a == needle)
    }

    #[test]
    fn parses_every_real_fixture() {
        for (name, vj) in [
            ("1.21.4", v1214()),
            ("1.20.1", v1201()),
            ("1.16.5", v1165()),
            ("1.12.2", v1122()),
            ("1.7.10", v1710()),
        ] {
            assert_eq!(vj.id, name);
            assert_eq!(vj.kind, "release");
            assert_eq!(
                vj.main_class.as_deref(),
                Some("net.minecraft.client.main.Main")
            );
            assert!(
                vj.downloads
                    .as_ref()
                    .and_then(|d| d.client.as_ref())
                    .is_some(),
                "{name} client jar"
            );
            assert!(vj.asset_index.is_some(), "{name} assetIndex");
            // assets 与 assetIndex.id 一致，asset_index_id() 两个来源都能取到
            assert_eq!(vj.asset_index_id(), Some(vj.assets.as_deref().unwrap()));
            assert!(!vj.libraries.is_empty(), "{name} libraries");
        }
    }

    #[test]
    fn java_runtime_mapping_matches_real_data() {
        // 真正决定"要哪个大版本"的是 majorVersion；component 只用于自动下载（P5）
        let cases = [
            (v1214(), "java-runtime-delta", 21),
            (v1201(), "java-runtime-gamma", 17),
            (v1165(), "jre-legacy", 8),
            (v1122(), "jre-legacy", 8),
            (v1710(), "jre-legacy", 8),
        ];
        for (vj, component, major) in cases {
            let java = vj
                .java_version
                .unwrap_or_else(|| panic!("{} javaVersion", vj.id));
            assert_eq!(java.component, component);
            assert_eq!(java.major_version, major);
        }
    }

    #[test]
    fn modern_jvm_args_are_platform_specific() {
        let vj = v1214();
        let v = vars();
        let mac = jvm_arguments(&vj, &mac_arm(), &no_features(), &v);
        let win = jvm_arguments(&vj, &win(), &no_features(), &v);
        let win32 = jvm_arguments(&vj, &win_x86(), &no_features(), &v);
        let nix = jvm_arguments(&vj, &linux(), &no_features(), &v);

        // macOS 专属：LWJGL 必须在第一线程
        assert!(has(&mac, "-XstartOnFirstThread"), "mac args: {mac:?}");
        assert!(!has(&win, "-XstartOnFirstThread"));
        assert!(!has(&nix, "-XstartOnFirstThread"));

        // windows 专属：Intel 驱动崩溃转储
        assert!(
            win.iter().any(|a| a.starts_with("-XX:HeapDumpPath=")),
            "win args: {win:?}"
        );
        assert!(!mac.iter().any(|a| a.starts_with("-XX:HeapDumpPath=")));

        // arch 规则：只有 32 位才加 -Xss1M
        assert!(has(&win32, "-Xss1M"));
        assert!(!has(&win, "-Xss1M"));

        // 三平台都要有的：natives 路径与 classpath（且 -cp 是最后两个 token）
        for args in [&mac, &win, &nix] {
            assert!(has(args, "-Djava.library.path=/natives"));
            assert!(has(args, "-Djna.tmpdir=/natives"));
            assert_eq!(args[args.len() - 2], "-cp");
            assert_eq!(args[args.len() - 1], "/cp/a.jar;/cp/b.jar");
        }
        // 陷阱：`-Dlog4j.configurationFile=${path}` **不在** arguments.jvm 里，它来自
        // logging.client.argument，由启动代码在下载完日志配置文件后单独追加。
        assert!(
            !mac.iter().any(|a| a.contains("log4j")),
            "logging argument must not come from arguments.jvm: {mac:?}"
        );
        assert_eq!(
            vj.logging
                .as_ref()
                .unwrap()
                .client
                .as_ref()
                .unwrap()
                .argument,
            "-Dlog4j.configurationFile=${path}"
        );
    }

    #[test]
    fn modern_game_args_substitute_and_honor_features() {
        let vj = v1214();
        let v = vars();
        let plain = game_arguments(&vj, &win(), &no_features(), &v);
        assert!(has(&plain, "--username"));
        assert!(has(&plain, "Alice"));
        assert!(has(&plain, "--gameDir"));
        assert!(has(&plain, "/game"));
        assert!(!has(&plain, "--demo"));
        assert!(
            !plain.iter().any(|a| a == "--width"),
            "no resolution without the feature"
        );

        let mut demo = no_features();
        demo.insert("is_demo_user".to_string(), true);
        assert!(has(&game_arguments(&vj, &win(), &demo, &v), "--demo"));

        let mut res = no_features();
        res.insert("has_custom_resolution".to_string(), true);
        let mut v2 = v.clone();
        v2.insert("resolution_width".to_string(), "1920".to_string());
        v2.insert("resolution_height".to_string(), "1080".to_string());
        let args = game_arguments(&vj, &win(), &res, &v2);
        assert!(has(&args, "--width") && has(&args, "1920") && has(&args, "1080"));

        // 没有留下任何未替换的占位符（未知键会被原样保留，正好便于断言）
        assert!(
            !plain.iter().any(|a| a.contains("${")),
            "unsubstituted placeholder: {plain:?}"
        );
    }

    #[test]
    fn legacy_versions_use_minecraft_arguments() {
        let v = vars();
        for vj in [v1122(), v1710()] {
            assert!(vj.arguments.is_none(), "{} should be legacy", vj.id);
            let game = game_arguments(&vj, &win(), &no_features(), &v);
            assert!(has(&game, "--username") && has(&game, "Alice"));
            assert!(has(&game, "--assetIndex"));
            // legacy 没有 arguments.jvm，回落到官方那组默认参数
            let jvm = jvm_arguments(&vj, &win(), &no_features(), &v);
            assert!(has(&jvm, "-Djava.library.path=/natives"));
            assert!(has(&jvm, "-Dminecraft.launcher.brand=MythicCompass"));
            assert_eq!(jvm[jvm.len() - 2], "-cp");
            assert!(!game.iter().any(|a| a.contains("${")), "{:?}", game);
            assert!(!jvm.iter().any(|a| a.contains("${")), "{:?}", jvm);
        }
        // 1.7.10 独有 ${user_properties}
        assert!(has(
            &game_arguments(&v1710(), &win(), &no_features(), &v),
            "{}"
        ));
    }

    #[test]
    fn legacy_natives_are_extracted_and_modern_ones_are_classpath() {
        let f = no_features();

        // 1.12.2 / Windows：lwjgl-platform 的 natives-windows 要解压，且不进 classpath
        let win_libs = resolve_libraries(&v1122(), &win(), &f);
        let native = win_libs
            .iter()
            .find(|l| {
                l.path
                    .contains("lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar")
            })
            .expect("windows natives for 1.12.2");
        match &native.kind {
            LibraryKind::Native { exclude } => assert_eq!(exclude, &vec!["META-INF/".to_string()]),
            other => panic!("legacy natives must be extracted, got {other:?}"),
        }
        assert!(native.url.starts_with("https://libraries.minecraft.net/"));
        assert_eq!(native.sha1.len(), 40);
        assert!(native.size > 0);

        // 同一条库在 macOS 上被 rules 的 disallow-osx 排除，改用 osx 专属的 2.9.2 那条
        let mac_libs = resolve_libraries(&v1122(), &mac_old(), &f);
        assert!(
            !mac_libs
                .iter()
                .any(|l| l.path.contains("2.9.4-nightly-20150209-natives-osx")),
            "osx must not get the disallowed 2.9.4 natives"
        );
        assert!(
            mac_libs
                .iter()
                .any(|l| l.path.contains("2.9.2-nightly-20140822-natives-osx")),
            "osx should get the 2.9.2 natives"
        );
        // Linux 上两条都不该出现
        let nix_libs = resolve_libraries(&v1122(), &linux(), &f);
        assert!(!nix_libs.iter().any(|l| l.path.contains("natives-osx")));

        // 1.7.10：natives-windows-${arch} 在 x64 上展开成 natives-windows-64
        let old = resolve_libraries(&v1710(), &win(), &f);
        assert!(
            old.iter().any(|l| l.path.contains("natives-windows-64")),
            "1.7.10 arch substitution: {:?}",
            old.iter().map(|l| &l.path).collect::<Vec<_>>()
        );

        // 1.21.4：现代 LWJGL3 全是 classpath 条目（无 natives/extract 块），一条都不解压
        let modern = resolve_libraries(&v1214(), &win(), &f);
        assert!(modern.iter().all(|l| l.kind == LibraryKind::Classpath));
        assert!(
            modern
                .iter()
                .any(|l| l.path.contains("lwjgl-3.3.3-natives-windows.jar")),
            "modern natives ride the classpath"
        );
        // macOS arm64 的原生包按 os.name 放行（版本 JSON 里没有 arch 规则）
        let mac_modern = resolve_libraries(&v1214(), &mac_arm(), &f);
        assert!(
            mac_modern
                .iter()
                .any(|l| l.path.contains("natives-macos-arm64"))
        );
        assert!(
            !mac_modern
                .iter()
                .any(|l| l.path.contains("natives-windows"))
        );
    }

    #[test]
    fn resolved_libraries_carry_checksums() {
        let f = no_features();
        for (os, vj) in [
            (win(), v1214()),
            (mac_arm(), v1201()),
            (linux(), v1165()),
            (win(), v1122()),
        ] {
            let libs = resolve_libraries(&vj, &os, &f);
            assert!(!libs.is_empty(), "{} on {:?}", vj.id, os.name);
            for l in &libs {
                assert!(!l.path.is_empty(), "empty path in {}", vj.id);
                assert!(l.url.starts_with("http"), "bad url {:?}", l.url);
                assert_eq!(l.sha1.len(), 40, "missing sha1 for {} in {}", l.path, vj.id);
                assert!(l.size > 0, "missing size for {} in {}", l.path, vj.id);
            }
        }
    }

    #[test]
    fn fabric_profile_has_no_downloads_block() {
        // Fabric/Quilt 生成的库把 sha1/size 放在库对象上、url 是 Maven 基址，路径要自己拼。
        // 而且**不是每条都给校验和**（intermediary 与 fabric-loader 就没有）：
        // 解析结果用空 sha1 表示"无从校验"，下载器据此跳过校验而不是拿空串当哈希。
        let f = no_features();
        let libs = resolve_libraries(&fabric(), &win(), &f);
        assert_eq!(libs.len(), 8);
        let mut with_checksum = 0;
        for l in &libs {
            assert_eq!(l.kind, LibraryKind::Classpath);
            assert!(
                l.url.starts_with("https://maven.fabricmc.net/"),
                "{:?}",
                l.url
            );
            assert!(l.path.ends_with(".jar"));
            assert!(l.sha1.is_empty() || l.sha1.len() == 40, "{:?}", l);
            if !l.sha1.is_empty() {
                with_checksum += 1;
            }
        }
        assert_eq!(
            with_checksum, 6,
            "asm/mixin carry checksums, intermediary/loader do not"
        );
        assert!(
            libs.iter()
                .any(|l| l.path == "org/ow2/asm/asm/9.10.1/asm-9.10.1.jar")
        );
        // 带构建元数据的版本号（0.17.5+mixin.0.8.7）也要能拼出正确路径
        assert!(libs.iter().any(|l| l.path
            == "net/fabricmc/sponge-mixin/0.17.5+mixin.0.8.7/sponge-mixin-0.17.5+mixin.0.8.7.jar"));
        assert!(libs.iter().any(|l| l.path.contains("fabric-loader")));
    }

    #[test]
    fn merge_fabric_child_over_vanilla_parent() {
        let merged = merge(v1214(), fabric());
        // 身份来自子版本
        assert_eq!(merged.id, "fabric-loader-0.20.1-1.21.4");
        assert_eq!(
            merged.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
        assert_eq!(merged.inherits_from.as_deref(), Some("1.21.4"));
        // client jar 与资源都来自父版本
        assert_eq!(merged.jar_version(), "1.21.4");
        assert!(merged.downloads.is_some());
        assert_eq!(merged.asset_index_id(), Some("19"));
        assert!(merged.logging.is_some());
        assert_eq!(merged.java_version.as_ref().unwrap().major_version, 21);
        // 库是"子在前 + 父在后"，并按 Maven 身份去重（子赢）：
        // 原版带 asm 9.6、Fabric 带 9.10.1，重复的那一份必须被去掉
        assert_eq!(
            merged.libraries.len(),
            8 + 113 - 1,
            "父子只有 org.ow2.asm:asm 一条重叠"
        );
        assert!(merged.libraries[0].name.contains("asm"));
        assert!(
            merged.libraries[8..]
                .iter()
                .any(|l| l.name.starts_with("com.mojang:"))
        );
        // game 参数子先父后；jvm 用子的
        let v = vars();
        let game = game_arguments(&merged, &win(), &no_features(), &v);
        let parent_game = game_arguments(&v1214(), &win(), &no_features(), &v);
        assert!(game.len() >= parent_game.len());
        assert_eq!(
            &game[game.len() - parent_game.len()..],
            parent_game.as_slice()
        );
        // jvm 参数是父 + 子拼接：父的 natives/classpath 与子的 Fabric 标记都要在，
        // 且 -cp 只能出现一次（子覆盖父会让启动命令缺 classpath，游戏起不来）
        let jvm = jvm_arguments(&merged, &win(), &no_features(), &v);
        assert!(has(&jvm, "-Djava.library.path=/natives"), "{jvm:?}");
        assert!(
            jvm.iter().any(|a| a.starts_with("-DFabricMcEmu=")),
            "child jvm args must survive the merge: {jvm:?}"
        );
        assert_eq!(jvm.iter().filter(|a| a.as_str() == "-cp").count(), 1);
        assert!(
            has(&jvm, "/cp/a.jar;/cp/b.jar"),
            "classpath must come from the parent: {jvm:?}"
        );
    }

    #[test]
    fn merge_dedupes_libraries_by_maven_identity_with_the_child_winning() {
        // 真实重叠：原版 1.21.4 带 org.ow2.asm:asm:9.6，Fabric 带 9.10.1。
        // 两份都留在 classpath 上时，NeoForge 的 BootstrapLauncher 会直接抛
        // "Module named org.objectweb.asm was already on the JVMs module path ...
        //  but class-path contains it at ...asm-9.6.jar"（拿官方 installer 的产物
        // 真跑启动时看到的报错）。Fabric 只是没做这个检查，所以一直没炸。
        let merged = merge(v1214(), fabric());
        let asm: Vec<&str> = merged
            .libraries
            .iter()
            .filter(|l| library_key(&l.name) == "org.ow2.asm:asm")
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(asm.len(), 1, "同一个逻辑库只能留一份：{asm:?}");
        assert!(
            asm[0].ends_with(":9.10.1"),
            "留下来的必须是子版本的（classpath 上子在前，先加载）：{}",
            asm[0]
        );
        // Fabric 独有的 asm 子构件不受影响（原版没有它们，不构成重复）
        for a in ["asm-analysis", "asm-commons", "asm-tree", "asm-util"] {
            assert!(
                merged
                    .libraries
                    .iter()
                    .any(|l| l.name.starts_with(&format!("org.ow2.asm:{a}:"))),
                "{a} 不该被去掉"
            );
        }
        // classifier 不同的条目是**不同的文件**，必须都留着：
        // 去掉 natives 分片的话 LWJGL 会加载不到本地库
        let lwjgl: Vec<&str> = merged
            .libraries
            .iter()
            .filter(|l| l.name.starts_with("org.lwjgl:lwjgl:"))
            .map(|l| l.name.as_str())
            .collect();
        assert!(lwjgl.contains(&"org.lwjgl:lwjgl:3.3.3"), "{lwjgl:?}");
        assert!(
            lwjgl.iter().any(|n| n.ends_with(":natives-windows")),
            "natives 分片不能被当成重复去掉：{lwjgl:?}"
        );
        assert!(
            lwjgl.iter().any(|n| n.ends_with(":natives-linux")),
            "{lwjgl:?}"
        );
        // 整个列表里没有重复的键
        let keys: Vec<String> = merged
            .libraries
            .iter()
            .map(|l| library_key(&l.name))
            .collect();
        let mut seen = std::collections::HashSet::new();
        let dupes: Vec<&String> = keys.iter().filter(|k| !seen.insert(*k)).collect();
        assert!(dupes.is_empty(), "合并后仍有重复的库：{dupes:?}");
    }

    #[test]
    fn library_key_ignores_version_but_keeps_classifier() {
        assert_eq!(library_key("org.ow2.asm:asm:9.6"), "org.ow2.asm:asm");
        assert_eq!(library_key("org.ow2.asm:asm:9.10.1"), "org.ow2.asm:asm");
        assert_ne!(
            library_key("org.lwjgl:lwjgl:3.3.3"),
            library_key("org.lwjgl:lwjgl:3.3.3:natives-windows"),
            "classifier 不同的必须算两个库"
        );
        assert_eq!(
            library_key("net.neoforged:neoform:1.21.4-20241203.161809:mappings@txt"),
            "net.neoforged:neoform:mappings@txt"
        );
        // 段数不足的畸形坐标原样返回，不要 panic（数据来自服务端）
        assert_eq!(library_key("weird"), "weird");
        assert_eq!(library_key("a:b"), "a:b");
    }

    #[test]
    fn loader_provides_game_jar_is_read_from_the_data_not_a_version_table() {
        // NeoForge 的真实 version.json（官方 installer 写出的那份）
        let merged_neoforge = merge(v1214(), neoforge());
        assert!(
            loader_provides_game_jar(&merged_neoforge),
            "NeoForge 的游戏 jar 由 fml_loader 自己按坐标找，classpath 里不能放"
        );
        // 子版本单独看也成立（合并顺序不该影响这个判断）
        assert!(loader_provides_game_jar(&neoforge()));

        // 原版、Fabric 走 KnotClient / 原版 Main，必须有原版 jar 在 classpath 上
        assert!(!loader_provides_game_jar(&v1214()));
        assert!(!loader_provides_game_jar(&merge(v1214(), fabric())));
        // Forge 1.12 及更早用 --tweakClass，同样要原版 jar
        assert!(!loader_provides_game_jar(&v1122()));
        assert!(!loader_provides_game_jar(&v1710()));
    }

    #[test]
    fn neoforge_child_args_survive_the_merge() {
        // --launchTarget 是 ModLauncher 的入口参数；少了它 fml_loader 拿到 null，
        // 在 ImmediateWindowHandler.load 里 NPE（真跑出来的栈）
        let merged = merge(v1214(), neoforge());
        let v = vars();
        let game = game_arguments(&merged, &win(), &no_features(), &v);
        let i = game
            .iter()
            .position(|a| a == "--launchTarget")
            .expect("--launchTarget 必须在游戏参数里");
        assert_eq!(game[i + 1], "neoforgeclient");
        for flag in [
            "--fml.neoForgeVersion",
            "--fml.fmlVersion",
            "--fml.mcVersion",
            "--fml.neoFormVersion",
        ] {
            assert!(game.iter().any(|a| a == flag), "{flag} 丢了：{game:?}");
        }
        // 子版本的参数排在父版本之前（既有约定），原版的 --username 等仍然在
        let username = game
            .iter()
            .position(|a| a == "--username")
            .expect("父版本的游戏参数不能丢");
        assert!(
            i < username,
            "子先父后：--launchTarget 在 {i}，--username 在 {username}"
        );

        // JVM 参数：NeoForge 的模块路径那一套与父版本的 -cp / -Djava.library.path 都要在
        let jvm = jvm_arguments(&merged, &win(), &no_features(), &v);
        assert!(has(&jvm, "-p"), "模块路径参数丢了：{jvm:?}");
        assert!(
            has(&jvm, "--add-modules"),
            "BootstrapLauncher 需要 --add-modules：{jvm:?}"
        );
        assert_eq!(jvm.iter().filter(|a| a.as_str() == "-cp").count(), 1);
        assert!(
            has(&jvm, "-Djava.library.path=/natives"),
            "父版本的 natives 路径不能被子版本覆盖掉：{jvm:?}"
        );
    }

    #[test]
    fn neoforge_merge_dedupes_asm_and_keeps_the_child_version() {
        // 原版带 asm 9.6、NeoForge 带 9.8 并把 9.8 放进模块路径（-p）。
        // 两份同时在 classpath 上时 BootstrapLauncher 直接抛
        // "Module named org.objectweb.asm was already on the JVMs module path ...
        //  but class-path contains it at ...asm-9.6.jar"（真跑出来的报错）。
        let merged = merge(v1214(), neoforge());
        let asm: Vec<&str> = merged
            .libraries
            .iter()
            .filter(|l| library_key(&l.name) == "org.ow2.asm:asm")
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(asm.len(), 1, "{asm:?}");
        assert!(asm[0].ends_with(":9.8"), "留下的必须是子版本的：{}", asm[0]);
        // 子版本的库排在父版本之前
        assert!(
            merged.libraries[0].name.starts_with("net.neoforged"),
            "{}",
            merged.libraries[0].name
        );
    }

    #[test]
    fn rule_semantics_last_match_wins() {
        let os = win();
        let f = no_features();
        let rule = |action: RuleAction, os_name: Option<&str>| Rule {
            action,
            os: os_name.map(|n| OsRule {
                name: Some(n.to_string()),
                version: None,
                arch: None,
            }),
            features: None,
        };

        // 无规则 = 允许
        assert!(rules_allow(None, &os, &f));
        // 有规则但都不命中 = 拒绝
        assert!(!rules_allow(
            Some(&[rule(RuleAction::Allow, Some("osx"))]),
            &os,
            &f
        ));
        // 最后一条命中的说了算
        assert!(!rules_allow(
            Some(&[
                rule(RuleAction::Allow, None),
                rule(RuleAction::Disallow, Some("windows"))
            ]),
            &os,
            &f
        ));
        assert!(rules_allow(
            Some(&[
                rule(RuleAction::Disallow, Some("windows")),
                rule(RuleAction::Allow, None)
            ]),
            &os,
            &f
        ));
    }

    #[test]
    fn rule_semantics_features_arch_and_os_version() {
        let f = no_features();
        let mut demo = Features::new();
        demo.insert("is_demo_user".to_string(), true);

        let feature_rule = Rule {
            action: RuleAction::Allow,
            os: None,
            features: Some(HashMap::from([("is_demo_user".to_string(), true)])),
        };
        assert!(rules_allow(
            Some(std::slice::from_ref(&feature_rule)),
            &win(),
            &demo
        ));
        assert!(!rules_allow(
            Some(std::slice::from_ref(&feature_rule)),
            &win(),
            &f
        ));

        // arch 规则：JSON 里只出现过 "x86"
        let x86_rule = Rule {
            action: RuleAction::Allow,
            os: Some(OsRule {
                name: None,
                version: None,
                arch: Some("x86".to_string()),
            }),
            features: None,
        };
        assert!(rules_allow(
            Some(std::slice::from_ref(&x86_rule)),
            &win_x86(),
            &f
        ));
        assert!(!rules_allow(
            Some(std::slice::from_ref(&x86_rule)),
            &win(),
            &f
        ));

        // os.version 是正则：1.16.5 用它把老 macOS 分开处理
        let old_mac = Rule {
            action: RuleAction::Allow,
            os: Some(OsRule {
                name: Some("osx".to_string()),
                version: Some("^10\\.".to_string()),
                arch: None,
            }),
            features: None,
        };
        let old_mac_rules = Some(std::slice::from_ref(&old_mac));
        assert!(rules_allow(old_mac_rules, &mac_old(), &f));
        assert!(!rules_allow(old_mac_rules, &mac_arm(), &f));
        assert!(!rules_allow(old_mac_rules, &linux(), &f));

        // 非法正则不该 panic，只当不命中
        let broken = Rule {
            action: RuleAction::Allow,
            os: Some(OsRule {
                name: None,
                version: Some("[".to_string()),
                arch: None,
            }),
            features: None,
        };
        assert!(!rules_allow(
            Some(std::slice::from_ref(&broken)),
            &win(),
            &f
        ));
    }

    #[test]
    fn log4shell_mitigation_range_is_exact() {
        // 1.18.2 起 Mojang 换了修好的 log4j；1.7–1.18.1 需要缓解参数
        for v in [
            "1.7.10", "1.8.9", "1.12.2", "1.16.5", "1.17", "1.18", "1.18.1",
        ] {
            assert!(needs_log4shell_mitigation(v), "{v} should be mitigated");
        }
        for v in [
            "1.18.2",
            "1.19",
            "1.20.1",
            "1.21.4",
            "26.4-snapshot-6",
            "",
            "rd-132211",
            "1.6.4",
        ] {
            assert!(
                !needs_log4shell_mitigation(v),
                "{v} should not be mitigated"
            );
        }
    }

    #[test]
    fn version_parts_handles_missing_and_odd_segments() {
        assert_eq!(version_parts("1.21.4"), Some((1, 21, 4)));
        assert_eq!(version_parts("1.18"), Some((1, 18, 0)));
        assert_eq!(version_parts("1.18.2-pre1"), Some((1, 18, 2)));
        assert_eq!(version_parts("26.4-snapshot-6"), Some((26, 4, 0)));
        assert_eq!(version_parts("rd-132211"), None);
        assert_eq!(version_parts(""), None);
    }

    #[test]
    fn substitute_keeps_unknown_placeholders() {
        let mut v = HashMap::new();
        v.insert("a".to_string(), "1".to_string());
        assert_eq!(substitute("--x=${a}", &v), "--x=1");
        assert_eq!(substitute("${a}${a}", &v), "11");
        assert_eq!(substitute("${nope}", &v), "${nope}");
        assert_eq!(substitute("no placeholders", &v), "no placeholders");
        assert_eq!(substitute("trailing ${", &v), "trailing ${");
    }

    #[test]
    fn maven_paths() {
        assert_eq!(
            maven_path("org.lwjgl.lwjgl:lwjgl:2.9.4-nightly-20150209").unwrap(),
            "org/lwjgl/lwjgl/lwjgl/2.9.4-nightly-20150209/lwjgl-2.9.4-nightly-20150209.jar"
        );
        assert_eq!(
            maven_path("org.lwjgl:lwjgl:3.3.3:natives-windows").unwrap(),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-windows.jar"
        );
        assert!(maven_path("nonsense").is_none());
        assert!(maven_path("a:b:c:d:e").is_none());
    }

    #[test]
    fn asset_index_and_client_download_survive_parsing() {
        let vj = v1214();
        let index = vj.asset_index.clone().unwrap();
        assert_eq!(index.id, "19");
        assert_eq!(index.sha1.len(), 40);
        assert!(index.url.ends_with("/19.json"));
        assert!(index.total_size.unwrap_or(0) > 400_000_000);
        let client = vj.downloads.unwrap().client.unwrap();
        assert!(client.url.ends_with("/client.jar"));
        assert_eq!(client.sha1.len(), 40);
        assert!(client.size > 20_000_000);
        // logging 的 argument 与文件都要能取到（Log4Shell 缓解与日志配置依赖它）
        let logging = vj.logging.unwrap().client.unwrap();
        assert_eq!(logging.argument, "-Dlog4j.configurationFile=${path}");
        assert_eq!(logging.kind, "log4j2-xml");
        assert!(logging.file.url.contains("client-1.21.2.xml"));
    }
}
