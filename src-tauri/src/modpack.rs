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
use std::path::Path;

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
        tasks.push(FileTask {
            url: url.clone(),
            dest,
            sha1: f.hashes.get("sha1").cloned().unwrap_or_default(),
            size: f.file_size,
        });
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
