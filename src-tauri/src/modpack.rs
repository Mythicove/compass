//! `.mrpack`（Modrinth 整合包）导入。
//!
//! 格式取自六个真包的实测（Fabulously Optimized / Adrenaline / Sodium Plus /
//! FastCraft / Optimized fps / Create+），不是照规范文档写的：
//! - 顶层字段**顺序不固定**，部分包多一个 `summary`，所以解析不能依赖顺序、要忽略未知字段
//!   （Hoard 自己的导出还多一个包级 `env`，规范里没有）；
//! - `dependencies` 的值是裸版本字符串，实测出现过 `fabric-loader`、`neoforge`、`minecraft`；
//! - `files[].env` 实测有 `required`/`optional`/`unsupported`，**还有规范外的 `unknown`**
//!   （Create+ 里有一个）；
//! - `hashes` 实测每个文件都同时给 sha1 与 sha512，`downloads` 都只有一个 CDN 地址。
//!
//! 安全边界：包里的 `path` 与 zip 条目名都决定我们往哪写，两者一律过
//! `archive::safe_rel_path`；下载只接受 http/https。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::ApiClient;
use crate::archive;
use crate::error::Error;
use crate::install::Installer;
use crate::instance::{self, Instance, InstanceLoader};
use crate::loader::{self, LoaderKind};
use crate::net::{FileTask, ProgressSink};
use crate::paths::Paths;

/// 整合包索引在 zip 里的固定位置。
pub const INDEX_ENTRY: &str = "modrinth.index.json";

/// 客户端安装要应用的覆盖目录；`server-overrides/` 是服务端专用，客户端跳过。
const CLIENT_OVERRIDE_DIRS: [&str; 2] = ["overrides/", "client-overrides/"];
const SERVER_OVERRIDE_DIR: &str = "server-overrides/";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrpackEnv {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub server: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrpackFile {
    pub path: String,
    #[serde(default)]
    pub hashes: HashMap<String, String>,
    #[serde(default)]
    pub downloads: Vec<String>,
    #[serde(default)]
    pub file_size: u64,
    /// 规范里可选。**缺省按"两侧都必需"处理**——Hoard 自己的导出就没有这个字段。
    #[serde(default)]
    pub env: Option<MrpackEnv>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MrpackIndex {
    pub format_version: u32,
    pub game: String,
    pub version_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub files: Vec<MrpackFile>,
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
}

/// 一个文件在**客户端**安装时的处置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    Required,
    /// 作者标为可选。**仍然装上**（决定见 `install`），但要在结果里单独报出来。
    Optional,
    /// `unsupported`：明确不该出现在这一侧，跳过。
    Unsupported,
    /// 规范外的值（实测 Create+ 里有 `unknown`）。按 required 装，但记一条警告：
    /// 静默吞掉一个我们看不懂的值，等它出问题就查不出是哪儿来的。
    Unrecognized(String),
}

impl Disposition {
    pub fn installs(&self) -> bool {
        !matches!(self, Disposition::Unsupported)
    }
}

pub fn disposition(f: &MrpackFile) -> Disposition {
    let Some(env) = &f.env else {
        return Disposition::Required;
    };
    match env.client.as_str() {
        "required" | "" => Disposition::Required,
        "optional" => Disposition::Optional,
        "unsupported" => Disposition::Unsupported,
        other => Disposition::Unrecognized(other.to_string()),
    }
}

/// `dependencies` 的键 → loader。规范里的键名，实测见过前四个。
fn loader_of(key: &str) -> Option<LoaderKind> {
    match key {
        "fabric-loader" => Some(LoaderKind::Fabric),
        "quilt-loader" => Some(LoaderKind::Quilt),
        "forge" => Some(LoaderKind::Forge),
        "neoforge" => Some(LoaderKind::Neoforge),
        _ => None,
    }
}

/// 从 `dependencies` 里取出游戏版本与 loader。
///
/// 返回（游戏版本, loader, 警告）。游戏版本缺失或是 `*` 时返回 None——
/// 装不了就是装不了，猜一个版本比拒绝更糟。Hoard 在项目没填 game_versions 时
/// 就会写 `"*"`，所以这条不是理论情况。
pub fn resolve_dependencies(
    deps: &HashMap<String, String>,
) -> (Option<String>, Option<(LoaderKind, String)>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut loader = None;
    for (key, value) in deps {
        if key == "minecraft" {
            continue;
        }
        match loader_of(key) {
            Some(kind) => {
                if loader.is_some() {
                    warnings.push(format!(
                        "整合包声明了多个 loader，只用第一个（另有 {key}={value}）"
                    ));
                } else {
                    loader = Some((kind, value.clone()));
                }
            }
            // liteloader / modloader / risugamis-modloader 这类我们不装：
            // 说清楚比静默忽略好，用户才知道为什么进去少东西
            None => warnings.push(format!("不支持的 loader 依赖 {key}={value}，已忽略")),
        }
    }
    let minecraft = deps
        .get("minecraft")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != "*");
    if minecraft.is_none() {
        // 说清是哪一种：写成 "*" 是来源没指定游戏版本（Hoard 在项目没填
        // game_versions 时就这么写），完全没有这个键则是包本身不完整
        warnings.push(
            match deps.get("minecraft").map(String::as_str) {
                Some("*") => {
                    "整合包把 minecraft 版本写成了 \"*\"（来源没有指定游戏版本），无法据此安装"
                }
                _ => "整合包没有声明 minecraft 版本，无法据此安装",
            }
            .into(),
        );
    }
    (minecraft, loader, warnings)
}

/// 装之前给用户看的东西。**先验后解**：整合包决定我们下载什么、往哪写，
/// 让用户在任何东西落盘之前看见它要做什么，和实例导入是同一条理由。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackInfo {
    pub name: String,
    /// 整合包自己的版本号（不是游戏版本），如 `8.2.0`
    pub pack_version: String,
    pub summary: Option<String>,
    pub format_version: u32,
    pub minecraft: Option<String>,
    pub loader: Option<LoaderKind>,
    pub loader_version: Option<String>,
    pub files_total: usize,
    pub files_required: usize,
    pub files_optional: usize,
    /// `env.client = unsupported`，客户端安装会跳过
    pub files_skipped: usize,
    /// 会下载的文件总字节数（不含 overrides 里的）
    pub bytes: u64,
    pub override_entries: usize,
    pub warnings: Vec<String>,
}

pub fn read_index(archive_path: &Path) -> Result<MrpackIndex, Error> {
    let Some(text) = archive::read_text_entry(archive_path, INDEX_ENTRY)? else {
        return Err(Error::Invalid(format!(
            "{} is not a Modrinth modpack: no {INDEX_ENTRY} at its root",
            archive_path.display()
        )));
    };
    let index: MrpackIndex = serde_json::from_str(&text)
        .map_err(|e| Error::Config(format!("{INDEX_ENTRY} is not readable: {e}")))?;
    if index.game != "minecraft" {
        return Err(Error::Invalid(format!(
            "modpack declares game {:?}, only \"minecraft\" is supported",
            index.game
        )));
    }
    if index.format_version != 1 {
        return Err(Error::Invalid(format!(
            "modpack formatVersion {} is not supported (only 1)",
            index.format_version
        )));
    }
    Ok(index)
}

pub fn inspect(archive_path: &Path) -> Result<ModpackInfo, Error> {
    let index = read_index(archive_path)?;
    let (minecraft, loader, mut warnings) = resolve_dependencies(&index.dependencies);

    let mut required = 0usize;
    let mut optional = 0usize;
    let mut skipped = 0usize;
    let mut bytes = 0u64;
    for f in &index.files {
        match disposition(f) {
            Disposition::Required => {
                required += 1;
                bytes += f.file_size;
            }
            Disposition::Optional => {
                optional += 1;
                bytes += f.file_size;
            }
            Disposition::Unsupported => skipped += 1,
            Disposition::Unrecognized(value) => {
                required += 1;
                bytes += f.file_size;
                warnings.push(format!(
                    "{} 的 env.client 是规范外的 {value:?}，按 required 处理",
                    f.path
                ));
            }
        }
        if archive::safe_rel_path(&f.path).is_err() {
            warnings.push(format!("文件路径 {:?} 不安全，安装时会拒绝", f.path));
        }
    }

    let (override_entries, _) = override_entries(archive_path)?;

    Ok(ModpackInfo {
        name: index.name,
        pack_version: index.version_id,
        summary: index.summary,
        format_version: index.format_version,
        minecraft,
        loader: loader.as_ref().map(|(k, _)| *k),
        loader_version: loader.map(|(_, v)| v),
        files_total: index.files.len(),
        files_required: required,
        files_optional: optional,
        files_skipped: skipped,
        bytes,
        override_entries,
        warnings,
    })
}

/// 数一下客户端要应用的覆盖条目（跳过目录条目与服务端专用目录）。
fn override_entries(archive_path: &Path) -> Result<(usize, Vec<String>), Error> {
    let file = std::fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    let mut names = Vec::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if name.ends_with('/') {
            continue;
        }
        if let Some(prefix) = CLIENT_OVERRIDE_DIRS.iter().find(|d| name.starts_with(**d)) {
            names.push((prefix.to_string(), name));
        }
    }
    Ok((names.len(), names.into_iter().map(|(_, n)| n).collect()))
}

/// 安装结果。分类计数是重点：装了多少必需、多少可选、跳过多少、覆盖了什么，
/// 用户要能看出"这个包本来就少了些东西"与"我们装失败了"的区别。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackInstall {
    pub instance: Instance,
    pub downloaded: usize,
    pub bytes: u64,
    pub optional_installed: usize,
    pub skipped_unsupported: usize,
    pub override_files: usize,
    /// 包里覆盖了已存在的文件（相对游戏目录）。整合包优先，但要把被覆盖的报出来。
    pub overwritten: Vec<String>,
    pub failed: Vec<String>,
    pub warnings: Vec<String>,
    /// 装完之后对整个实例做一次依赖体检。
    ///
    /// 整合包一次装几十个 mod，逐个走安装时那道闸门既慢（每个文件都要重扫一遍目录）
    /// 又不合适——两个 mod 互相冲突不该让整包安装失败。所以这里不拦，只把结果
    /// 一起交出去：装完就知道缺什么、谁跟谁不兼容，而不是等用户想起来去点"检查"。
    pub deps: crate::content::DependencyReport,
}

/// 装一个 `.mrpack`：建实例 → 装游戏版本与 loader → 应用 overrides → 下载 files。
///
/// **slug 由调用方给，绝不取自包里**（同 `instance::import`）：包里的名字是别人
/// 机器上的，让它决定本地路径等于让一个外部文件决定我们往哪写。
///
/// 失败时把实例目录整个删掉：装到一半的整合包比"没装"更难处理，用户看到的会是
/// 一个缺文件的实例，而不是一个明确的失败。
pub async fn install(
    paths: &Paths,
    api: &ApiClient,
    installer: &Installer,
    archive_path: &Path,
    slug: &str,
    name: &str,
    progress: Option<ProgressSink>,
) -> Result<ModpackInstall, Error> {
    // 先验后解：格式不对、版本装不了，都在写盘之前拦住
    let index = read_index(archive_path)?;
    let (minecraft, loader, mut warnings) = resolve_dependencies(&index.dependencies);
    let Some(game) = minecraft else {
        return Err(Error::Invalid(
            "整合包没有可用的 minecraft 版本（缺失或写成了 \"*\"），无法安装".into(),
        ));
    };

    let dst = instance::instance_dir(paths, slug)?;
    if dst.exists() {
        return Err(Error::Invalid(format!("instance already exists: {slug}")));
    }

    // 1. 游戏版本 + loader。loader 必须在建实例之前装完：实例的 version_id 要存
    //    loader 的继承版本 id，那才是启动时真正用的版本
    installer.install(&game, progress.clone()).await?;
    let mut version_id = game.clone();
    let mut record: Option<InstanceLoader> = None;
    if let Some((kind, loader_version)) = loader {
        let installed = loader::install(
            installer,
            api,
            kind,
            &game,
            &loader_version,
            progress.clone(),
        )
        .await?;
        version_id = installed.id.clone();
        record = Some(InstanceLoader {
            kind,
            version: loader_version,
            game_version: game.clone(),
        });
    }

    let mut inst = Instance {
        slug: slug.to_string(),
        name: if name.trim().is_empty() {
            index.name.clone()
        } else {
            name.trim().to_string()
        },
        version_id,
        created_at: crate::account::now_unix(),
        loader: record,
        ..Instance::default()
    };
    if inst.name.trim().is_empty() {
        inst.name = slug.to_string();
    }

    // 从这里开始会往磁盘写东西，失败要回滚
    if let Err(e) = instance::save(paths, &inst) {
        let _ = std::fs::remove_dir_all(&dst);
        return Err(e);
    }
    let game_dir = match instance::game_dir(paths, slug) {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dst);
            return Err(e);
        }
    };

    let result = populate(
        installer,
        archive_path,
        &index,
        &game_dir,
        &mut warnings,
        progress,
    )
    .await;
    // 失败要回滚：装到一半的整合包比"没装"更难处理，用户看到的是一个缺文件的实例，
    // 而不是一个明确的失败。实例配置在 populate 之前就已经落盘，所以这里要连目录一起删。
    let outcome = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dst);
            return Err(e);
        }
    };

    // 来源簿记落盘。放在回滚之后：装失败时实例目录整个删掉，簿记也跟着没了，
    // 不会留下一份指向不存在文件的记录
    instance::save_provenance(paths, slug, &outcome.provenance)?;

    // 装完做一次全实例体检。loader 类型要用**装完之后的**那个（可能是 None → 原版），
    // 因为 minecraft / fabricloader 这些 id 是不是"平台已提供"取决于它
    let deps = crate::content::check_instance(&game_dir, inst.loader.as_ref().map(|l| l.kind));

    Ok(ModpackInstall {
        instance: inst,
        downloaded: outcome.downloaded,
        bytes: outcome.bytes,
        optional_installed: outcome.optional_installed,
        skipped_unsupported: outcome.skipped,
        override_files: outcome.override_files,
        overwritten: outcome.overwritten,
        failed: outcome.failed,
        warnings,
        deps,
    })
}

/// 应用 overrides + 下载 files 的结果。用结构体而不是七元组：
/// 元组的位置含义只有写的人记得，调用方数错一个位置编译器也不会拦。
#[derive(Debug, Default)]
struct PopulateOutcome {
    downloaded: usize,
    bytes: u64,
    optional_installed: usize,
    skipped: usize,
    override_files: usize,
    overwritten: Vec<String>,
    failed: Vec<String>,
    /// 每个下载文件的来源，装完写进实例的簿记（导出 `.mrpack` 时用它生成 `files[]`）
    provenance: crate::instance::ProvenanceMap,
}

/// 应用 overrides + 下载 files。
///
/// Downloader 取自 `installer`，不另外传一个：两个 Downloader 就意味着两套
/// 并发数与镜像配置，而它们本该是同一份。
async fn populate(
    installer: &Installer,
    archive_path: &Path,
    index: &MrpackIndex,
    game_dir: &Path,
    warnings: &mut Vec<String>,
    progress: Option<ProgressSink>,
) -> Result<PopulateOutcome, Error> {
    std::fs::create_dir_all(game_dir)?;

    // 覆盖先应用，再下载 files：files 带 sha1，是包里更权威的那一份，
    // 同名时让它赢
    let (override_files, mut overwritten) = apply_overrides(archive_path, game_dir)?;

    let mut tasks = Vec::new();
    let mut provenance = crate::instance::ProvenanceMap::new();
    let mut optional_installed = 0usize;
    let mut skipped = 0usize;
    for f in &index.files {
        let disp = disposition(f);
        if let Disposition::Unrecognized(value) = &disp {
            warnings.push(format!(
                "{} 的 env.client 是规范外的 {value:?}，按 required 处理",
                f.path
            ));
        }
        if !disp.installs() {
            skipped += 1;
            continue;
        }
        if disp == Disposition::Optional {
            optional_installed += 1;
        }
        let rel = archive::safe_rel_path(&f.path).map_err(|_| {
            Error::Invalid(format!(
                "refusing unsafe path {:?} from {}",
                f.path,
                archive_path.display()
            ))
        })?;
        let dest = game_dir.join(&rel);
        let Some(url) = f.downloads.iter().find(|u| is_http(u)) else {
            // 没有可用地址不是致命错误：整合包里偶尔有作者删掉的 mod。
            // 记进 failed 让用户看得见，比整包失败或静默少一个文件都好
            warnings.push(format!("{} 没有可用的 http(s) 下载地址，已跳过", f.path));
            continue;
        };
        if dest.exists() {
            overwritten.push(f.path.clone());
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let sha1 = f.hashes.get("sha1").cloned().unwrap_or_default();
        tasks.push(FileTask {
            url: url.clone(),
            dest,
            sha1: sha1.clone(),
            size: f.file_size,
        });
        // 来源簿记：导出 `.mrpack` 时靠它把文件写成 `files[]`（只存地址与哈希）
        // 而不是把字节塞进 overrides/。键一律用 `/`，与索引里的 path 一致。
        provenance.insert(
            f.path.replace('\\', "/"),
            crate::instance::Provenance {
                url: url.clone(),
                sha1,
                size: f.file_size,
            },
        );
    }

    let summary = installer.downloader().download_all(tasks, progress).await;
    Ok(PopulateOutcome {
        // 本来就有的也算"到位了"：重装同一个包时下载器按 sha1 认出已存在并跳过
        downloaded: summary.downloaded + summary.skipped,
        bytes: summary.bytes,
        optional_installed,
        skipped,
        override_files,
        overwritten,
        failed: summary.failed,
        provenance,
    })
}

fn is_http(url: &str) -> bool {
    matches!(
        url::Url::parse(url).map(|u| u.scheme().to_string()).ok(),
        Some(s) if s == "http" || s == "https"
    )
}

/// 把 `overrides/` 与 `client-overrides/` 解到游戏目录，返回（文件数, 被覆盖的相对路径）。
///
/// 每个条目都过 `safe_rel_path`：整合包是别人做的，条目名里带 `../` 就等于
/// 让它往游戏目录之外写文件。
fn apply_overrides(archive_path: &Path, game_dir: &Path) -> Result<(usize, Vec<String>), Error> {
    let file = std::fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    let mut names = Vec::new();
    for i in 0..zip.len() {
        let name = zip.by_index(i)?.name().to_string();
        if name.ends_with('/') || name.starts_with(SERVER_OVERRIDE_DIR) {
            continue;
        }
        if let Some(prefix) = CLIENT_OVERRIDE_DIRS.iter().find(|d| name.starts_with(**d)) {
            names.push((prefix.to_string(), name));
        }
    }
    let mut count = 0usize;
    let mut overwritten = Vec::new();
    for (prefix, name) in names {
        let rel = name.strip_prefix(prefix.as_str()).unwrap_or(&name);
        if rel.is_empty() {
            continue;
        }
        let rel = archive::safe_rel_path(rel).map_err(|_| {
            Error::Invalid(format!(
                "refusing unsafe entry {name:?} in {}",
                archive_path.display()
            ))
        })?;
        let dest = game_dir.join(&rel);
        if dest.exists() {
            overwritten.push(rel.to_string_lossy().replace('\\', "/"));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut entry = zip.by_name(&name)?;
        let part = crate::net::part_path(&dest);
        let mut out = std::fs::File::create(&part)?;
        std::io::copy(&mut entry, &mut out)?;
        drop(out);
        std::fs::rename(&part, &dest)?;
        count += 1;
    }
    Ok((count, overwritten))
}

/// 供测试与 UI 复用：整合包里的 loader 键名。
pub fn known_loader_keys() -> Vec<&'static str> {
    vec!["fabric-loader", "quilt-loader", "forge", "neoforge"]
}

// ---------------------------------------------------------------------------
// 导出
// ---------------------------------------------------------------------------

/// `dependencies` 里的 loader 键名（与 `loader_of` 互为反向）。
fn loader_key(kind: LoaderKind) -> &'static str {
    match kind {
        LoaderKind::Fabric => "fabric-loader",
        LoaderKind::Quilt => "quilt-loader",
        LoaderKind::Forge => "forge",
        LoaderKind::Neoforge => "neoforge",
    }
}

/// mod 自己声明的运行侧 → mrpack 的 `env`。
///
/// 用 mod 元数据里的 `environment`，不是一律写"两侧都要"：实测 46 个真 jar 里
/// 30 个是 `client`，一律写 required 会让服务端整合包装上 30 个客户端 mod。
/// 也没有一律写"只客户端"——那会让 `*`（13 个）与 server-only 的 mod 被漏掉。
fn env_of(environment: Option<&str>) -> (String, String) {
    match environment.unwrap_or("*").trim() {
        "client" => ("required".into(), "unsupported".into()),
        "server" => ("unsupported".into(), "required".into()),
        // `*`、空、以及我们不认识的值都按"两侧都要"：多装一个比少装一个安全，
        // 而且这与导入侧对缺失 `env` 的处理一致
        _ => ("required".into(), "required".into()),
    }
}

/// 不进整合包的用户数据。存档是**本人的进度**，日志与崩溃报告是本机的诊断，
/// 截图更是纯个人文件；把它们打进分享包既臃肿又泄露。
const EXPORT_SKIP_DIRS: [&str; 4] = ["saves", "logs", "crash-reports", "screenshots"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportEnv {
    client: String,
    server: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportFile {
    path: String,
    hashes: HashMap<String, String>,
    env: ExportEnv,
    downloads: Vec<String>,
    file_size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportIndex {
    format_version: u32,
    game: String,
    version_id: String,
    name: String,
    files: Vec<ExportFile>,
    dependencies: HashMap<String, String>,
}

/// 导出结果。分类计数是重点：用户要能看出"为什么这个包这么大"。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackExport {
    pub path: String,
    /// 进 `files[]` 的（有来源簿记，且 sha1 与磁盘一致）
    pub files: usize,
    /// 进 `overrides/` 的实体文件（字节真的写进包里）
    pub override_files: usize,
    pub bytes: u64,
    /// 有簿记但 sha1 与磁盘对不上（文件被换过）→ 退回 overrides
    pub stale: Vec<String>,
    /// 被排除的用户数据目录
    pub skipped_dirs: Vec<String>,
}

/// 把实例导出成 `.mrpack`。
///
/// 分两类装：**有来源簿记**的文件写成 `files[]`（只存地址与 sha1，安装方自己下），
/// 其余的把字节塞进 `overrides/`。差别是体积：实测 46 个 mod 的实例，
/// 全走 overrides 约 200MB，全走 files 索引只有约 30KB。
///
/// 簿记的 sha1 会与磁盘上的文件**重新核对**，对不上就退回 overrides：
/// 用户换过一个 mod 文件之后，簿记里的地址与哈希都是旧的，照着写出去
/// 会让安装方下到一个与包里声明不符的东西。
pub async fn export(
    paths: &Paths,
    slug: &str,
    dest: &Path,
    pack_version: &str,
) -> Result<ModpackExport, Error> {
    let inst = instance::load(paths, slug)?;
    let game_dir = instance::game_dir(paths, slug)?;
    if !game_dir.is_dir() {
        return Err(Error::NotFound(format!(
            "{} has no game directory yet",
            game_dir.display()
        )));
    }
    let instance_dir = instance::instance_dir(paths, slug)?;
    // 目标不能在被打包的目录里：否则边写边读，包会一路长到自己把自己吃进去
    if dest.starts_with(&instance_dir) {
        return Err(Error::Invalid(format!(
            "refusing to export into the instance itself: {}",
            dest.display()
        )));
    }

    let provenance = instance::load_provenance(paths, slug);
    let mut files: Vec<ExportFile> = Vec::new();
    let mut overrides: Vec<(String, PathBuf)> = Vec::new();
    let mut stale = Vec::new();
    let mut bytes = 0u64;

    // 被跳过的用户数据目录：`walk_game_dir` 根本不下潜到它们里面（存档可能几个 GB，
    // 走进去再过滤等于白读一遍），所以这里单独按"目录存在"报告
    let skipped: Vec<String> = EXPORT_SKIP_DIRS
        .iter()
        .filter(|d| game_dir.join(d).is_dir())
        .map(|d| d.to_string())
        .collect();

    for (rel, abs) in walk_game_dir(&game_dir)? {
        let size = std::fs::metadata(&abs)?.len();
        bytes += size;
        let prov = provenance.get(&rel).filter(|p| !p.url.trim().is_empty());
        // 有簿记也要与磁盘核对：sha1 对不上说明文件被换过，簿记已经过期
        let usable = match prov {
            Some(p) => {
                let actual = crate::net::sha1_of_file(&abs).await.unwrap_or_default();
                if !p.sha1.is_empty() && actual.eq_ignore_ascii_case(&p.sha1) {
                    Some(p)
                } else {
                    stale.push(rel.clone());
                    None
                }
            }
            None => None,
        };
        match usable {
            Some(p) => {
                let (client, server) = if rel.starts_with("mods/") {
                    let env = crate::content::read_mod_meta(&abs).and_then(|m| m.environment);
                    env_of(env.as_deref())
                } else {
                    env_of(None)
                };
                let mut hashes = HashMap::new();
                hashes.insert("sha1".to_string(), p.sha1.clone());
                files.push(ExportFile {
                    path: rel,
                    hashes,
                    env: ExportEnv { client, server },
                    downloads: vec![p.url.clone()],
                    file_size: p.size.max(size),
                });
            }
            None => overrides.push((rel, abs)),
        }
    }

    // 依赖：游戏版本 + loader。两者都取自实例记录，不猜。
    let mut dependencies = HashMap::new();
    let game_version = inst
        .loader
        .as_ref()
        .map(|l| l.game_version.clone())
        .unwrap_or_else(|| inst.version_id.clone());
    dependencies.insert("minecraft".to_string(), game_version);
    if let Some(l) = &inst.loader {
        dependencies.insert(loader_key(l.kind).to_string(), l.version.clone());
    }

    let index = ExportIndex {
        format_version: 1,
        game: "minecraft".to_string(),
        // 空版本号会让安装方分不清这是哪一版；用实例名兜底也比留空好
        version_id: if pack_version.trim().is_empty() {
            inst.version_id.clone()
        } else {
            pack_version.trim().to_string()
        },
        name: if inst.name.trim().is_empty() {
            slug.to_string()
        } else {
            inst.name.clone()
        },
        files,
        dependencies,
    };

    write_pack(dest, &index, &overrides)?;
    Ok(ModpackExport {
        path: dest.to_string_lossy().into_owned(),
        files: index.files.len(),
        override_files: overrides.len(),
        bytes,
        stale,
        skipped_dirs: skipped,
    })
}

/// 把索引与 overrides 写成一个 `.mrpack`。
fn write_pack(
    dest: &Path,
    index: &ExportIndex,
    overrides: &[(String, PathBuf)],
) -> Result<(), Error> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 先写 .part 再改名：打包到一半失败不该留下一个看着完整、其实缺尾巴的 zip
    let part = crate::net::part_path(dest);
    {
        let file = std::fs::File::create(&part)?;
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let text = serde_json::to_vec_pretty(index)
            .map_err(|e| Error::Config(format!("modpack index is not serializable: {e}")))?;
        zip.start_file(INDEX_ENTRY, opts)?;
        std::io::Write::write_all(&mut zip, &text)?;
        for (rel, abs) in overrides {
            // 条目名一律用 `/`：zip 的规范如此，而 Windows 上 rel 可能带 `\`
            let entry = format!("overrides/{}", rel.replace('\\', "/"));
            zip.start_file(&entry, opts)?;
            let mut src = std::fs::File::open(abs)?;
            std::io::copy(&mut src, &mut zip)?;
        }
        zip.finish()?;
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// 遍历游戏目录，返回（相对路径用 `/` 分隔, 绝对路径）。跳过 `archive::SKIP_DIRS`
/// 与临时文件——那些是下载器留下的半成品，不该进包。
fn walk_game_dir(root: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}{name}")
            };
            let path = entry.path();
            if ft.is_dir() {
                if crate::archive::SKIP_DIRS.contains(&name.as_str())
                    || EXPORT_SKIP_DIRS.contains(&name.as_str())
                {
                    continue;
                }
                stack.push((path, format!("{rel}/")));
            } else if !crate::archive::skip_file(&name) {
                out.push((rel, path));
            }
        }
    }
    // 稳定顺序：包的内容不该随目录遍历顺序变化（否则同样的实例两次导出字节不同）
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 真包的路径（Modrinth 上 Fabulously Optimized 8.2.0 / Minecraft 1.21.4，183KB）。
    /// 存整个 `.mrpack` 而不是抽出来的 index：抽出来的那份是**派生产物**，
    /// 与真包会各自漂移，而 overrides 目录的形狀只有整包里才有。
    fn real_pack() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mrpack-fabulously-optimized.mrpack")
    }

    fn real_index() -> MrpackIndex {
        read_index(&real_pack()).unwrap_or_else(|e| panic!("real pack should parse: {e}"))
    }

    fn file(path: &str, client: &str) -> MrpackFile {
        MrpackFile {
            path: path.into(),
            hashes: HashMap::new(),
            downloads: vec!["https://cdn.modrinth.com/x.jar".into()],
            file_size: 10,
            env: Some(MrpackEnv {
                client: client.into(),
                server: "required".into(),
            }),
        }
    }

    #[test]
    fn parses_the_real_index_and_reads_its_dependencies() {
        let index = real_index();
        assert_eq!(index.format_version, 1);
        assert_eq!(index.game, "minecraft");
        assert_eq!(index.version_id, "8.2.0");
        assert_eq!(index.name, "Fabulously Optimized");
        assert_eq!(index.files.len(), 48);
        assert_eq!(
            index.dependencies.get("minecraft").map(String::as_str),
            Some("1.21.4")
        );
        assert_eq!(
            index.dependencies.get("fabric-loader").map(String::as_str),
            Some("0.19.3")
        );

        let (mc, loader, warnings) = resolve_dependencies(&index.dependencies);
        assert_eq!(mc.as_deref(), Some("1.21.4"));
        assert_eq!(loader, Some((LoaderKind::Fabric, "0.19.3".to_string())));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn every_real_file_has_a_usable_checksum_and_url() {
        let index = real_index();
        for f in &index.files {
            assert!(
                f.hashes.get("sha1").is_some_and(|s| s.len() == 40),
                "{} 没有可用的 sha1：{:?}",
                f.path,
                f.hashes
            );
            assert!(
                f.downloads.iter().any(|u| is_http(u)),
                "{} 没有 http(s) 地址：{:?}",
                f.path,
                f.downloads
            );
            assert!(f.file_size > 0, "{} 的 fileSize 是 0", f.path);
            assert!(
                archive::safe_rel_path(&f.path).is_ok(),
                "{} 过不了路径检查",
                f.path
            );
            // 实测这个包里全是 mods/ 与 resourcepacks/，都落在游戏目录下
            assert!(
                f.path.starts_with("mods/") || f.path.starts_with("resourcepacks/"),
                "没预料到的落点：{}",
                f.path
            );
        }
    }

    #[test]
    fn inspect_of_the_real_pack_reports_what_the_ui_will_show() {
        let info = inspect(&real_pack()).unwrap();
        assert_eq!(info.name, "Fabulously Optimized");
        assert_eq!(info.pack_version, "8.2.0");
        assert_eq!(info.format_version, 1);
        assert_eq!(info.minecraft.as_deref(), Some("1.21.4"));
        assert_eq!(info.loader, Some(LoaderKind::Fabric));
        assert_eq!(info.loader_version.as_deref(), Some("0.19.3"));
        // 实测这个包 48 个文件全是 required/required，没有 optional 也没有 unsupported
        assert_eq!(info.files_total, 48);
        assert_eq!(info.files_required, 48);
        assert_eq!(info.files_optional, 0);
        assert_eq!(info.files_skipped, 0);
        assert!(
            info.bytes > 10_000_000,
            "48 个 mod 不止这点字节：{}",
            info.bytes
        );
        // 这个真包没有 summary 字段（六个样本里有四个有），所以必须可选
        assert_eq!(info.summary, None);
        // overrides/ 下有 62 个文件（zip 共 64 条，含 1 个目录条目与 index 本身）
        assert_eq!(info.override_entries, 62, "{info:?}");
        assert!(info.warnings.is_empty(), "{:?}", info.warnings);
    }

    #[test]
    fn env_values_decide_what_a_client_install_takes() {
        assert_eq!(
            disposition(&file("mods/a.jar", "required")),
            Disposition::Required
        );
        assert_eq!(
            disposition(&file("mods/a.jar", "optional")),
            Disposition::Optional
        );
        assert_eq!(
            disposition(&file("mods/a.jar", "unsupported")),
            Disposition::Unsupported
        );
        // 规范外的值（实测 Create+ 里有 `unknown`）：装上，但必须能被认出来
        assert_eq!(
            disposition(&file("mods/a.jar", "unknown")),
            Disposition::Unrecognized("unknown".into())
        );
        assert!(disposition(&file("mods/a.jar", "unknown")).installs());
        assert!(!disposition(&file("mods/a.jar", "unsupported")).installs());

        // 没有 env 字段 = 两侧都必需（Hoard 的导出就没有这个字段）
        let mut bare = file("mods/a.jar", "required");
        bare.env = None;
        assert_eq!(disposition(&bare), Disposition::Required);
    }

    #[test]
    fn dependency_resolution_refuses_to_guess_a_game_version() {
        let mut deps: HashMap<String, String> = HashMap::new();
        deps.insert("minecraft".into(), "*".into());
        deps.insert("fabric-loader".into(), "0.19.3".into());
        let (mc, loader, warnings) = resolve_dependencies(&deps);
        assert_eq!(mc, None, "\"*\" 不是能装的版本");
        assert_eq!(loader, Some((LoaderKind::Fabric, "0.19.3".to_string())));
        assert!(
            warnings.iter().any(|w| w.contains('*')),
            "必须说清是通配符的问题：{warnings:?}"
        );

        // 我们不装的 loader 要报出来，不能静默少东西
        let mut deps2: HashMap<String, String> = HashMap::new();
        deps2.insert("minecraft".into(), "1.21.4".into());
        deps2.insert("liteloader".into(), "1.12.2".into());
        let (mc, loader, warnings) = resolve_dependencies(&deps2);
        assert_eq!(mc.as_deref(), Some("1.21.4"));
        assert_eq!(loader, None);
        assert!(
            warnings.iter().any(|w| w.contains("liteloader")),
            "{warnings:?}"
        );

        // 四个规范里的 loader 键都要认
        for (key, kind) in [
            ("fabric-loader", LoaderKind::Fabric),
            ("quilt-loader", LoaderKind::Quilt),
            ("forge", LoaderKind::Forge),
            ("neoforge", LoaderKind::Neoforge),
        ] {
            let mut d: HashMap<String, String> = HashMap::new();
            d.insert("minecraft".into(), "1.21.4".into());
            d.insert(key.into(), "1.2.3".into());
            let (_, loader, _) = resolve_dependencies(&d);
            assert_eq!(loader, Some((kind, "1.2.3".to_string())), "{key}");
        }
    }

    #[test]
    fn non_minecraft_or_unknown_format_is_rejected_with_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("pack.mrpack");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("modrinth.index.json"),
            r#"{"formatVersion":1,"game":"minetest","versionId":"1","name":"x"}"#,
        )
        .unwrap();
        archive::zip_dir(&src, &pack, &[]).unwrap();
        let err = read_index(&pack).unwrap_err().to_string();
        assert!(err.contains("minetest"), "{err}");
        assert!(err.contains("minecraft"), "{err}");

        // formatVersion 2 出来时，拒绝并说清是版本问题，好过按 1 解析出一堆空值
        std::fs::write(
            src.join("modrinth.index.json"),
            r#"{"formatVersion":2,"game":"minecraft","versionId":"1","name":"x"}"#,
        )
        .unwrap();
        archive::zip_dir(&src, &pack, &[]).unwrap();
        let err = read_index(&pack).unwrap_err().to_string();
        assert!(err.contains("formatVersion 2"), "{err}");

        // 根本不是整合包（我们自己的实例导出包也是 zip）
        std::fs::write(src.join("modrinth.index.json"), b"").unwrap();
        std::fs::remove_file(src.join("modrinth.index.json")).unwrap();
        archive::zip_dir(&src, &pack, &[]).unwrap();
        let err = read_index(&pack).unwrap_err().to_string();
        assert!(err.contains(INDEX_ENTRY), "{err}");
    }

    /// 造一个 `.mrpack`。`entries` 的名字用 `/`（zip 里就是这个分隔符）。
    fn build_pack(dir: &Path, index: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join(INDEX_ENTRY), index).unwrap();
        for (name, bytes) in entries {
            let p = src.join(name.replace('/', std::path::MAIN_SEPARATOR_STR));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, bytes).unwrap();
        }
        let pack = dir.join("pack.mrpack");
        archive::zip_dir(&src, &pack, &[]).unwrap();
        pack
    }

    /// 四种 env 各一个文件 + 三种 overrides 目录，用来钉住分类计数与目录取舍。
    fn mixed_pack(dir: &Path) -> PathBuf {
        let index = r#"{
            "formatVersion": 1, "game": "minecraft", "versionId": "1.0", "name": "Mixed",
            "summary": "a pack that exercises every branch",
            "dependencies": {"minecraft": "1.21.4", "fabric-loader": "0.19.3"},
            "files": [
              {"path":"mods/req.jar","hashes":{"sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"downloads":["https://cdn.example/req.jar"],"fileSize":11,"env":{"client":"required","server":"required"}},
              {"path":"mods/opt.jar","hashes":{"sha1":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},"downloads":["https://cdn.example/opt.jar"],"fileSize":22,"env":{"client":"optional","server":"optional"}},
              {"path":"mods/srv.jar","hashes":{"sha1":"cccccccccccccccccccccccccccccccccccccccc"},"downloads":["https://cdn.example/srv.jar"],"fileSize":33,"env":{"client":"unsupported","server":"required"}},
              {"path":"mods/odd.jar","hashes":{"sha1":"dddddddddddddddddddddddddddddddddddddd"},"downloads":["https://cdn.example/odd.jar"],"fileSize":44,"env":{"client":"unknown","server":"unknown"}}
            ]
        }"#;
        build_pack(
            dir,
            index,
            &[
                ("overrides/config/a.toml", b"a"),
                ("overrides/mods/from-override.jar", b"o"),
                ("client-overrides/config/client-only.toml", b"c"),
                ("server-overrides/config/server-only.toml", b"s"),
                ("modrinth.index.json.unrelated", b"x"),
            ],
        )
    }

    #[tokio::test]
    async fn export_splits_known_files_from_the_rest() {
        // 三条分界都要验：有簿记且 sha1 对得上 → files[]（只存地址）；
        // 簿记过期（文件被换过）→ 退回 overrides 并记进 stale；
        // 用户数据（saves/logs/crash-reports/screenshots）→ 根本不进包。
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("data"));
        paths.ensure().unwrap();
        let inst = Instance {
            slug: "exp".into(),
            name: "Exported".into(),
            version_id: "fabric-loader-0.19.3-1.21.4".into(),
            created_at: 1,
            loader: Some(InstanceLoader {
                kind: LoaderKind::Fabric,
                version: "0.19.3".into(),
                game_version: "1.21.4".into(),
            }),
            ..Instance::default()
        };
        instance::save(&paths, &inst).unwrap();
        let game = instance::game_dir(&paths, "exp").unwrap();

        // 一个带 environment=client 的真 zip，用来验 env 取自 mod 自己的声明
        let src = dir.path().join("mod-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("fabric.mod.json"),
            r#"{ "id": "clientonly", "version": "1.0", "name": "Client Only",
                 "environment": "client" }"#,
        )
        .unwrap();
        std::fs::create_dir_all(game.join("mods")).unwrap();
        crate::archive::zip_dir(&src, &game.join("mods/clientonly.jar"), &[]).unwrap();
        std::fs::write(game.join("mods/stale.jar"), b"replaced-by-the-user").unwrap();
        std::fs::create_dir_all(game.join("config")).unwrap();
        std::fs::write(game.join("config/x.toml"), b"toml").unwrap();
        std::fs::write(game.join("options.txt"), b"fov:90").unwrap();
        std::fs::create_dir_all(game.join("saves/world")).unwrap();
        std::fs::write(game.join("saves/world/level.dat"), b"my-progress").unwrap();
        std::fs::create_dir_all(game.join("logs")).unwrap();
        std::fs::write(game.join("logs/latest.log"), b"secret-ish").unwrap();

        let good = crate::net::sha1_of_file(&game.join("mods/clientonly.jar"))
            .await
            .unwrap();
        let mut prov = crate::instance::ProvenanceMap::new();
        prov.insert(
            "mods/clientonly.jar".into(),
            crate::instance::Provenance {
                url: "https://cdn.example/clientonly.jar".into(),
                sha1: good.clone(),
                size: 1234,
            },
        );
        prov.insert(
            "mods/stale.jar".into(),
            crate::instance::Provenance {
                url: "https://cdn.example/stale.jar".into(),
                // 簿记里的哈希与磁盘对不上：用户换过这个文件
                sha1: "0".repeat(40),
                size: 1,
            },
        );
        prov.insert(
            "mods/gone.jar".into(),
            crate::instance::Provenance {
                url: "https://cdn.example/gone.jar".into(),
                sha1: "1".repeat(40),
                size: 1,
            },
        );
        instance::save_provenance(&paths, "exp", &prov).unwrap();

        let dest = dir.path().join("out.mrpack");
        let out = export(&paths, "exp", &dest, "1.0").await.unwrap();
        println!("{out:?}");
        assert_eq!(out.files, 1, "只有簿记对得上的那个进 files[]");
        // config/x.toml、options.txt、mods/stale.jar（簿记过期退回）；gone.jar 不在磁盘上
        assert_eq!(out.override_files, 3, "{out:?}");
        assert_eq!(out.stale, vec!["mods/stale.jar".to_string()], "{out:?}");
        assert_eq!(
            out.skipped_dirs,
            // 顺序跟 EXPORT_SKIP_DIRS 的声明序（saves 排第一：那是最重要的排除项）
            vec!["saves".to_string(), "logs".to_string()],
            "用户数据不该进包：{out:?}"
        );
        assert!(dest.is_file());

        let index = read_index(&dest).unwrap();
        assert_eq!(index.format_version, 1);
        assert_eq!(index.game, "minecraft");
        assert_eq!(index.version_id, "1.0");
        assert_eq!(index.name, "Exported");
        assert_eq!(index.files.len(), 1);
        assert_eq!(index.files[0].path, "mods/clientonly.jar");
        assert_eq!(
            index.files[0].downloads,
            vec!["https://cdn.example/clientonly.jar".to_string()]
        );
        assert_eq!(
            index.files[0].hashes.get("sha1").map(String::as_str),
            Some(good.as_str())
        );
        // env 来自 mod 自己声明的 environment=client，不是一律写"两侧都要"
        let env = index.files[0].env.as_ref().expect("files 应当带 env");
        assert_eq!(env.client, "required");
        assert_eq!(env.server, "unsupported");
        assert_eq!(
            index.dependencies.get("minecraft").map(String::as_str),
            Some("1.21.4"),
            "游戏版本取自实例记录，不是猜的"
        );
        assert_eq!(
            index.dependencies.get("fabric-loader").map(String::as_str),
            Some("0.19.3")
        );

        // 包里不该有用户数据，也不该把我们自己的簿记打进去
        let (_, entries) = override_entries(&dest).unwrap();
        assert!(
            !entries
                .iter()
                .any(|e| e.contains("saves") || e.contains("logs")),
            "{entries:?}"
        );
        assert!(
            !entries.iter().any(|e| e.contains("provenance")),
            "我们自己的簿记不该出现在别人的包里：{entries:?}"
        );
        assert!(
            entries.iter().any(|e| e == "overrides/mods/stale.jar"),
            "{entries:?}"
        );

        // 目标落在实例目录里必须被拒绝：边写边读会让包一路长到自己把自己吃进去
        let inside = instance::instance_dir(&paths, "exp")
            .unwrap()
            .join("bad.mrpack");
        let err = export(&paths, "exp", &inside, "1.0")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("into the instance itself"), "{err}");
    }

    #[test]
    fn inspect_counts_files_by_env_and_warns_about_the_odd_ones() {
        let dir = tempfile::tempdir().unwrap();
        let pack = mixed_pack(dir.path());
        let info = inspect(&pack).unwrap();

        assert_eq!(info.name, "Mixed");
        assert_eq!(info.pack_version, "1.0");
        assert_eq!(
            info.summary.as_deref(),
            Some("a pack that exercises every branch")
        );
        assert_eq!(info.minecraft.as_deref(), Some("1.21.4"));
        assert_eq!(info.loader, Some(LoaderKind::Fabric));
        assert_eq!(info.loader_version.as_deref(), Some("0.19.3"));
        assert_eq!(info.files_total, 4);
        // required 与规范外的 unknown 都算进"会装"，但 unknown 要单独报出来
        assert_eq!(info.files_required, 2, "{info:?}");
        assert_eq!(info.files_optional, 1);
        assert_eq!(info.files_skipped, 1, "unsupported 必须被数出来");
        assert_eq!(info.bytes, 11 + 22 + 44, "只算会下载的那些");
        // overrides/ 2 个 + client-overrides/ 1 个；server-overrides/ 与 index 都不算
        assert_eq!(info.override_entries, 3, "{info:?}");
        assert!(
            info.warnings
                .iter()
                .any(|w| w.contains("unknown") && w.contains("mods/odd.jar")),
            "规范外的 env 值必须出现在警告里：{:?}",
            info.warnings
        );
    }

    #[test]
    fn inspect_flags_a_pack_it_cannot_install() {
        let dir = tempfile::tempdir().unwrap();
        let pack = build_pack(
            dir.path(),
            r#"{"formatVersion":1,"game":"minecraft","versionId":"1","name":"No MC",
                "dependencies":{"minecraft":"*"},"files":[]}"#,
            &[],
        );
        let info = inspect(&pack).unwrap();
        assert_eq!(info.minecraft, None);
        assert!(
            info.warnings.iter().any(|w| w.contains('*')),
            "通配符版本要说清：{:?}",
            info.warnings
        );

        // 路径不安全的文件要在**安装之前**就能看见，不是等解压时才炸
        let dir2 = tempfile::tempdir().unwrap();
        let evil = build_pack(
            dir2.path(),
            r#"{"formatVersion":1,"game":"minecraft","versionId":"1","name":"Evil",
                "dependencies":{"minecraft":"1.21.4"},
                "files":[{"path":"../../escape.jar","hashes":{},"downloads":["https://cdn.example/e.jar"],"fileSize":1}]}"#,
            &[],
        );
        let info = inspect(&evil).unwrap();
        assert!(
            info.warnings.iter().any(|w| w.contains("escape.jar")),
            "{:?}",
            info.warnings
        );
    }

    #[test]
    fn client_overrides_are_applied_and_server_overrides_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let pack = mixed_pack(dir.path());
        let game = dir.path().join("game");
        std::fs::create_dir_all(&game).unwrap();

        let (count, overwritten) = apply_overrides(&pack, &game).unwrap();
        assert_eq!(count, 3, "overrides/ 2 个 + client-overrides/ 1 个");
        assert!(overwritten.is_empty(), "{overwritten:?}");
        assert_eq!(std::fs::read(game.join("config/a.toml")).unwrap(), b"a");
        assert_eq!(
            std::fs::read(game.join("mods/from-override.jar")).unwrap(),
            b"o"
        );
        assert_eq!(
            std::fs::read(game.join("config/client-only.toml")).unwrap(),
            b"c"
        );
        // 服务端专用的东西不该出现在客户端实例里
        assert!(
            !game.join("config/server-only.toml").exists(),
            "server-overrides/ 被应用了"
        );
        // 索引本身不是游戏内容
        assert!(!game.join(INDEX_ENTRY).exists());

        // 已存在的文件会被包里的覆盖，但必须报出来（用户的配置不是可以悄悄换掉的）。
        // 用一个干净目录、只预置一个文件：在刚跑过一遍的目录里测，三个都会被报成"覆盖"，
        // 那就分不清是"检测到了预置文件"还是"把自己刚写的又报了一遍"。
        let game2 = dir.path().join("game2");
        std::fs::create_dir_all(game2.join("config")).unwrap();
        std::fs::write(game2.join("config/a.toml"), b"mine").unwrap();
        let (count, overwritten) = apply_overrides(&pack, &game2).unwrap();
        assert_eq!(count, 3);
        assert_eq!(
            overwritten,
            vec!["config/a.toml".to_string()],
            "{overwritten:?}"
        );
        assert_eq!(
            std::fs::read(game2.join("config/a.toml")).unwrap(),
            b"a",
            "整合包优先"
        );
    }

    #[test]
    fn override_extraction_refuses_path_traversal() {
        // 整合包是别人做的：条目名里带 ../ 就等于让它往游戏目录之外写文件
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("packs")).unwrap();
        let pack = dir.path().join("packs").join("evil.mrpack");
        let f = std::fs::File::create(&pack).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file(INDEX_ENTRY, opts).unwrap();
        std::io::Write::write_all(
            &mut zw,
            br#"{"formatVersion":1,"game":"minecraft","versionId":"1","name":"x"}"#,
        )
        .unwrap();
        zw.start_file("overrides/../../escape.toml", opts).unwrap();
        std::io::Write::write_all(&mut zw, b"pwned").unwrap();
        zw.finish().unwrap();

        let game = dir.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        let err = apply_overrides(&pack, &game).unwrap_err().to_string();
        assert!(err.contains("refusing unsafe entry"), "{err}");
        assert!(
            !dir.path().join("escape.toml").exists(),
            "文件被写到了游戏目录之外"
        );
    }
}
