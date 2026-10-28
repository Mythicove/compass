//! 实例内容管理：mods / 资源包 / 光影 / 存档的扫描、元数据解析、启停、导入与删除。
//!
//! 设计上的两个决定：
//!
//! - **只有 mods 有"启用/禁用"**。资源包的启用状态是游戏自己写在 `options.txt` 里的，
//!   光影与存档同理（选中状态在各自的配置文件里）；启动器再造一个"启用"开关，
//!   UI 上说启用、游戏里却没生效，那是骗人的。其它内容类型只提供导入/删除/打开目录。
//! - **文件名一律经过 `validate_file_name`**。内容操作全部以"实例目录内的裸文件名"为参数，
//!   不接受路径：一旦允许分隔符，`../../.ssh/id_rsa` 就成了一次文件读取。
//!
//! mod 元数据来自 jar 内部（`fabric.mod.json` / `quilt.mod.json` / `META-INF/*mods.toml` /
//! `mcmod.info`）。解析失败一律降级为文件名显示——一个损坏的 jar 不该让整个 mods 列表变空。

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// 内容类型，字符串形式**就是 Minecraft 的目录名**：UI 不需要一层翻译，
/// 用户在文件管理器里看到的目录与这里的取值一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Mods,
    Resourcepacks,
    Shaderpacks,
    Saves,
}

impl Kind {
    pub const ALL: [Kind; 4] = [
        Kind::Mods,
        Kind::Resourcepacks,
        Kind::Shaderpacks,
        Kind::Saves,
    ];

    pub fn dir_name(self) -> &'static str {
        match self {
            Kind::Mods => "mods",
            Kind::Resourcepacks => "resourcepacks",
            Kind::Shaderpacks => "shaderpacks",
            Kind::Saves => "saves",
        }
    }

    pub fn parse(s: &str) -> Result<Kind, Error> {
        Self::ALL
            .into_iter()
            .find(|k| k.dir_name() == s)
            .ok_or_else(|| Error::Invalid(format!("unknown content kind: {s:?}")))
    }

    /// 这类内容是否支持启用/禁用（见模块注释：只有 mods）。
    pub fn supports_enable(self) -> bool {
        self == Kind::Mods
    }
}

/// jar 内的 mod 元数据。`loader` 可能多个（同一个 jar 同时发 Fabric 与 NeoForge 版）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModMeta {
    pub name: String,
    pub id: String,
    pub version: String,
    pub loaders: Vec<String>,
    /// **必需**依赖，每项是 `"<modId> <版本约束>"`（约束为 `*` 或没有时只留 modId）。
    /// 只收必需项：把 optional / incompatible 也列进来，用户会以为"缺了这个就起不来"。
    /// 这里只做展示，不做版本区间求值——那需要一套 semver range 解析器，
    /// 而"看起来判定了兼容性但判错"比"不判定"更糟（见 P3）。
    pub depends: Vec<String>,
}

/// 依赖数量上限。一个 jar 里的元数据是别人写的，不设上限就等于让别人的文件
/// 决定我们往 UI 塞多少字符串。
const MAX_DEPENDS: usize = 64;

/// 列表里的一行。`file_name` 是实例内容目录下的裸文件名（禁用时是 `disabled/` 下的名字）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentItem {
    pub kind: Kind,
    pub file_name: String,
    /// 元数据里的显示名；拿不到就用文件名去扩展名。
    pub display_name: String,
    pub size: u64,
    pub modified_at: u64,
    pub enabled: bool,
    /// jar 内解析出的元数据；None = 不是 mod jar，或元数据读不出来。
    /// 不 flatten：IPC 两侧都有类型定义，展平会让以后的字段名撞车变成静默的数据错乱。
    pub meta: Option<ModMeta>,
}

/// 一个内容目录的汇总（UI 顶栏用）。不 derive Default：`Kind` 没有"默认类型"，
/// 一个默认值等于允许构造出"不属于任何目录的统计"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentStats {
    pub kind: Kind,
    pub total: usize,
    pub enabled: usize,
    pub bytes: u64,
}

// ---------------------------------------------------------------------------
// 文件名校验
// ---------------------------------------------------------------------------

/// 内容文件名规则：非空、≤200 字符、不含路径分隔符与 `..`、不含控制字符。
/// 与 `instance::validate_slug` 的区别是**允许 `.`**（扩展名必须有），
/// 所以 `..` 要单独挡；两个规则各自只有一处实现。
pub fn validate_file_name(name: &str) -> Result<(), Error> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && !name.contains("..")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.chars().any(|c| c.is_control());
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "invalid content file name: {name:?}"
        )))
    }
}

/// 拼出内容目录（`<game_dir>/<kind>`）。
pub fn dir(game_dir: &Path, kind: Kind) -> PathBuf {
    game_dir.join(kind.dir_name())
}

/// mods 的"禁用"目录。Forge/Fabric 只扫 `mods/*.jar` 不递归，
/// 所以放进子目录就是禁用——这也是 HMCL/PCL 用的惯例，用户手动移动同样生效。
fn disabled_dir(game_dir: &Path) -> PathBuf {
    dir(game_dir, Kind::Mods).join("disabled")
}

fn unix(ts: std::io::Result<std::time::SystemTime>) -> u64 {
    ts.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 去掉扩展名与"已禁用"后缀，得到给用户看的兜底名字。
fn stem_of(file_name: &str) -> String {
    let base = file_name
        .strip_suffix(".disabled")
        .unwrap_or(file_name)
        .to_string();
    Path::new(&base)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or(base)
}

// ---------------------------------------------------------------------------
// mod 元数据解析
// ---------------------------------------------------------------------------

/// fabric `depends` 的值：可能是字符串（`">=0.16.0"`），也可能是字符串数组
/// （sodium 的 `"minecraft": ["1.21.4"]` 就是数组）。
///
/// 只按一种形状解析会让**整个** `fabric.mod.json` 解码失败，于是这个 mod 从
/// "有元数据"退化成"读不出名字"——比不解析依赖糟糕得多。真样本见
/// `tests/real_mods.rs`（sodium 0.6.13 同时用了两种写法）。
#[derive(Deserialize)]
#[serde(untagged)]
enum DepValue {
    One(String),
    Many(Vec<String>),
}

impl DepValue {
    fn constraint(&self) -> String {
        match self {
            DepValue::One(s) => s.clone(),
            DepValue::Many(v) => v.join(" "),
        }
    }
}

/// 用 BTreeMap 而不是 HashMap：依赖列表要按 modId 稳定排序，
/// 否则同一个 jar 每次扫描出来的顺序都可能不同，UI 上看着像在变。
#[derive(Deserialize)]
struct FabricModJson {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    depends: std::collections::BTreeMap<String, DepValue>,
}

#[derive(Deserialize)]
struct QuiltModJson {
    #[serde(default)]
    quilt_loader: Option<QuiltLoader>,
}

#[derive(Deserialize)]
struct QuiltLoader {
    #[serde(default)]
    id: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    metadata: Option<QuiltMetadata>,
    #[serde(default)]
    depends: Vec<QuiltDep>,
}

#[derive(Deserialize)]
struct QuiltMetadata {
    #[serde(default)]
    name: Option<String>,
}

/// quilt 的依赖项：裸字符串，或带 `versions` / `optional` 的对象。
#[derive(Deserialize)]
#[serde(untagged)]
enum QuiltDep {
    Id(String),
    Full {
        id: String,
        #[serde(default)]
        versions: Option<DepValue>,
        #[serde(default)]
        optional: bool,
    },
}

/// Forge 1.13+ / NeoForge 的 `mods.toml`。只取第一个 `[[mods]]`：
/// 多 mod 的 jar 极少，且第一个通常就是主 mod。
#[derive(Deserialize)]
struct ModsToml {
    #[serde(default)]
    mods: Vec<TomlMod>,
    /// 依赖挂在 `dependencies.<本 mod 的 modId>` 下（真样本：JEI 的 neoforge.mods.toml
    /// 写的是 `[[dependencies.jei]]`），所以要遍历这张表的所有值。
    #[serde(default)]
    dependencies: std::collections::BTreeMap<String, Vec<TomlDep>>,
}

#[derive(Deserialize)]
struct TomlMod {
    #[serde(default, rename = "modId")]
    mod_id: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct TomlDep {
    #[serde(default, rename = "modId")]
    mod_id: String,
    #[serde(default, rename = "versionRange")]
    version_range: String,
    /// NeoForge 用 `type`：required / optional / incompatible / discouraged
    #[serde(default, rename = "type")]
    dep_type: String,
    /// Forge 1.13+ 用 `mandatory`
    #[serde(default)]
    mandatory: Option<bool>,
}

impl TomlDep {
    /// 只算必需依赖。把 `incompatible` 也列进来的话，用户会以为"缺了它才起不来"，
    /// 而它的意思恰恰相反（装了才起不来）。
    fn required(&self) -> bool {
        if !self.dep_type.is_empty() {
            return self.dep_type == "required";
        }
        self.mandatory != Some(false)
    }
}

/// 1.12 及更早的 `mcmod.info`（一个数组）。
#[derive(Deserialize)]
struct McModInfo {
    #[serde(default)]
    modid: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

const METADATA_LIMIT: u64 = 512 * 1024;

/// 依赖项的统一格式化：`"<modId> <约束>"`，约束为 `*`（任意版本）时只留 modId。
/// 上限 `MAX_DEPENDS`——元数据是别人写的，不设上限等于让别人的文件决定 UI 里塞多少行。
///
/// **在这里统一排序**：fabric 的 depends 是对象（天然按 key 有序），
/// 而 toml 与 quilt 是数组（声明顺序）。不统一的话同一个 mod 的 Fabric 版与
/// NeoForge 版会给出不同顺序，看起来像列表在乱跳。排序只在这一处。
fn dep_list<I, S>(pairs: I) -> Vec<String>
where
    I: IntoIterator<Item = (S, String)>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = pairs
        .into_iter()
        .map(|(id, constraint)| {
            let id = id.as_ref().trim();
            let c = constraint.trim();
            if c.is_empty() || c == "*" {
                id.to_string()
            } else {
                format!("{id} {c}")
            }
        })
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out.truncate(MAX_DEPENDS);
    out
}

/// 从 jar 里读 mod 元数据。任何一步失败都返回 None（调用方降级为文件名）。
///
/// 解析顺序是 Fabric → Quilt → NeoForge → Forge → mcmod.info；
/// 同时存在时以靠前的那个为准（双端 jar 的 Fabric 元数据通常更完整），
/// 但 `loaders` 会记下所有找到的 loader。
pub fn read_mod_meta(path: &Path) -> Option<ModMeta> {
    let file = File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).ok()?;
    let names: Vec<String> = archive.file_names().map(str::to_string).collect();

    let mut loaders = Vec::new();
    let mut primary: Option<ModMeta> = None;
    let mut take = |meta: ModMeta, loader: &str, primary: &mut Option<ModMeta>| {
        loaders.push(loader.to_string());
        if primary.is_none() {
            *primary = Some(meta);
        }
    };

    for (name, loader) in [
        ("fabric.mod.json", "fabric"),
        ("quilt.mod.json", "quilt"),
        ("META-INF/neoforge.mods.toml", "neoforge"),
        ("META-INF/mods.toml", "forge"),
        ("mcmod.info", "legacy"),
    ] {
        if !names.iter().any(|n| n == name) {
            continue;
        }
        let Some(text) = read_entry(&mut archive, name) else {
            continue;
        };
        let meta = match name {
            "fabric.mod.json" => {
                serde_json::from_str::<FabricModJson>(&text)
                    .ok()
                    .map(|f| ModMeta {
                        name: f.name.unwrap_or_else(|| f.id.clone()),
                        id: f.id,
                        version: f.version.unwrap_or_default(),
                        loaders: Vec::new(),
                        depends: dep_list(
                            f.depends.into_iter().map(|(id, v)| (id, v.constraint())),
                        ),
                    })
            }
            "quilt.mod.json" => serde_json::from_str::<QuiltModJson>(&text)
                .ok()
                .and_then(|q| q.quilt_loader)
                .map(|l| {
                    let name = l
                        .metadata
                        .as_ref()
                        .and_then(|m| m.name.clone())
                        .unwrap_or_else(|| l.id.clone());
                    ModMeta {
                        name,
                        id: l.id,
                        version: l.version.unwrap_or_default(),
                        loaders: Vec::new(),
                        depends: dep_list(l.depends.into_iter().filter_map(|d| match d {
                            QuiltDep::Id(id) => Some((id, String::new())),
                            // optional 的依赖缺了也能跑，列出来只会让人以为必须装
                            QuiltDep::Full { optional: true, .. } => None,
                            QuiltDep::Full { id, versions, .. } => {
                                Some((id, versions.map(|v| v.constraint()).unwrap_or_default()))
                            }
                        })),
                    }
                }),
            "mcmod.info" => serde_json::from_str::<Vec<McModInfo>>(&text)
                .ok()
                .and_then(|v| v.into_iter().find(|m| !m.modid.is_empty()))
                .map(|m| ModMeta {
                    name: m.name.unwrap_or_else(|| m.modid.clone()),
                    id: m.modid,
                    version: m.version.unwrap_or_default(),
                    loaders: Vec::new(),
                    // mcmod.info 的 dependencies 只有 modId，没有版本约束
                    depends: dep_list(m.dependencies.into_iter().map(|id| (id, String::new()))),
                }),
            _ => text
                .parse::<toml::Table>()
                .ok()
                .and_then(|t| t.try_into::<ModsToml>().ok())
                .and_then(|t| {
                    // 依赖挂在 `dependencies.<本 mod 的 modId>` 下，可能有多个键
                    // （多 mod jar），所以把所有值都收进来，而不是只取自己那个
                    let deps: Vec<(String, String)> = t
                        .dependencies
                        .values()
                        .flatten()
                        .filter(|d| d.required() && !d.mod_id.is_empty())
                        .map(|d| (d.mod_id.clone(), d.version_range.clone()))
                        .collect();
                    let m = t.mods.into_iter().next()?;
                    Some(ModMeta {
                        name: m.display_name.unwrap_or_else(|| m.mod_id.clone()),
                        id: m.mod_id,
                        // Forge 常写 `${file.jarVersion}`，那是构建期占位符，见下面的兜底
                        version: m.version.unwrap_or_default(),
                        loaders: Vec::new(),
                        depends: dep_list(deps),
                    })
                }),
        };
        if let Some(mut meta) = meta {
            if meta.version.contains("${") || meta.version.trim().is_empty() {
                meta.version = manifest_version(&mut archive)
                    .or_else(|| {
                        Some(stem_of(
                            &path.file_name().unwrap_or_default().to_string_lossy(),
                        ))
                    })
                    .unwrap_or_default();
            }
            if meta.id.is_empty() {
                continue;
            }
            take(meta, loader, &mut primary);
        }
    }

    primary.map(|mut p| {
        p.loaders = loaders;
        p
    })
}

/// 读 zip 里的一个文本条目。超过 512KiB 直接放弃：元数据文件都是几百字节，
/// 读到那么大的东西说明这个 jar 不是我们要的（或是故意撑爆内存）。
fn read_entry<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    let mut entry = archive.by_name(name).ok()?;
    if entry.size() > METADATA_LIMIT {
        return None;
    }
    let mut buf = String::new();
    entry.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// `MANIFEST.MF` 的 `Implementation-Version`：Forge 的 `${file.jarVersion}` 在构建时被替换成它，
/// 所以这是"版本号显示为占位符"时的正确兜底。
fn manifest_version<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> Option<String> {
    let text = read_entry(archive, "META-INF/MANIFEST.MF")?;
    text.lines()
        .find_map(|line| line.strip_prefix("Implementation-Version:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// 扫描
// ---------------------------------------------------------------------------

/// 扫描一个内容目录。目录不存在返回空列表（新建实例还没有 mods 目录，那是常态）。
pub fn scan(game_dir: &Path, kind: Kind) -> Vec<ContentItem> {
    let base = dir(game_dir, kind);
    let mut items = list_dir(&base, kind, true);
    if kind == Kind::Mods {
        items.extend(list_dir(&disabled_dir(game_dir), kind, false));
    }
    items.sort_by(|a, b| {
        b.enabled.cmp(&a.enabled).then_with(|| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        })
    });
    items
}

/// 列出单个目录（不递归，`disabled/` 由调用方单独列）。
fn list_dir(base: &Path, kind: Kind, enabled: bool) -> Vec<ContentItem> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let file_name = e.file_name().to_str()?.to_string();
            // 编辑器的临时文件、macOS 的元数据文件、下载器的续传半成品都不是内容。
            // `.part` 尤其要挡：下载中断会把 `mods/foo.jar.part` 留在这儿，
            // 显示成一个"mod"的话，用户会以为装了个坏 mod。
            if file_name.starts_with('.')
                || file_name.ends_with(".tmp")
                || file_name.ends_with(".part")
            {
                return None;
            }
            let md = e.metadata().ok()?;
            // `.disabled` 后缀是另一种禁用惯例（用户手动改的），与 disabled/ 目录等效
            let on = enabled && !file_name.ends_with(".disabled");
            let path = e.path();
            let meta = if kind == Kind::Mods && file_name.ends_with(".jar") {
                read_mod_meta(&path)
            } else {
                None
            };
            let display_name = meta
                .as_ref()
                .map(|m| m.name.clone())
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| stem_of(&file_name));
            Some(ContentItem {
                kind,
                file_name,
                display_name,
                size: md.len(),
                modified_at: unix(md.modified()),
                enabled: on,
                meta,
            })
        })
        .collect()
}

pub fn stats(items: &[ContentItem], kind: Kind) -> ContentStats {
    ContentStats {
        kind,
        total: items.len(),
        enabled: items.iter().filter(|i| i.enabled).count(),
        bytes: items.iter().map(|i| i.size).sum(),
    }
}

// ---------------------------------------------------------------------------
// 启停 / 导入 / 删除
// ---------------------------------------------------------------------------

/// 一个内容文件的真实位置（启用时在 `<kind>/`，禁用时在 `<kind>/disabled/`）。
fn locate(game_dir: &Path, kind: Kind, file_name: &str) -> Result<PathBuf, Error> {
    validate_file_name(file_name)?;
    let active = dir(game_dir, kind).join(file_name);
    if active.is_file() {
        return Ok(active);
    }
    if kind == Kind::Mods {
        let off = disabled_dir(game_dir).join(file_name);
        if off.is_file() {
            return Ok(off);
        }
        // `x.jar.disabled` 也是禁用态的一种写法
        let suffixed = dir(game_dir, kind).join(format!("{file_name}.disabled"));
        if suffixed.is_file() {
            return Ok(suffixed);
        }
    }
    Err(Error::NotFound(format!(
        "{} not found in {}",
        file_name,
        kind.dir_name()
    )))
}

/// 启用/禁用一个 mod：在 `mods/` 与 `mods/disabled/` 之间移动。
///
/// 只有 mods 支持（见 `Kind::supports_enable`）——其它类型的启用状态由游戏自己的配置文件决定。
pub fn set_enabled(game_dir: &Path, file_name: &str, enabled: bool) -> Result<ContentItem, Error> {
    let kind = Kind::Mods;
    let from = locate(game_dir, kind, file_name)?;
    let current_name = from
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_name)
        .to_string();
    // 目标文件名：启用时去掉 `.disabled` 后缀，禁用时保持原名（惯例是放进 disabled/ 目录）
    let target_name = if enabled {
        current_name
            .strip_suffix(".disabled")
            .unwrap_or(&current_name)
            .to_string()
    } else {
        current_name.clone()
    };
    let to = if enabled {
        dir(game_dir, kind).join(&target_name)
    } else {
        disabled_dir(game_dir).join(&target_name)
    };
    if from == to {
        // 已经是目标状态：不报错，直接返回当前条目（UI 连点两次不该失败）
        return item_for(game_dir, kind, &to, enabled);
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if to.exists() {
        return Err(Error::Invalid(format!(
            "{} already exists; refusing to overwrite",
            to.file_name().unwrap_or_default().to_string_lossy()
        )));
    }
    std::fs::rename(&from, &to)?;
    // 改名之后按新名字报告（`x.jar.disabled` → `x.jar`）
    item_for(game_dir, kind, &to, enabled)
}

/// 从磁盘上的一个文件构造列表条目（下载完成后回报给 UI 用）。
pub fn item_for(
    game_dir: &Path,
    kind: Kind,
    path: &Path,
    enabled: bool,
) -> Result<ContentItem, Error> {
    let md = std::fs::metadata(path)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Invalid("content file has no usable name".into()))?
        .to_string();
    let meta = if kind == Kind::Mods && file_name.ends_with(".jar") {
        read_mod_meta(path)
    } else {
        None
    };
    let _ = game_dir;
    let display_name = meta
        .as_ref()
        .map(|m| m.name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| stem_of(&file_name));
    Ok(ContentItem {
        kind,
        file_name,
        display_name,
        size: md.len(),
        modified_at: unix(md.modified()),
        enabled,
        meta,
    })
}

/// 把外部文件（或文件夹）导入实例内容目录。返回实际写入的名字。
///
/// 文件夹是存档的常见形态（世界就是一个目录），走实例复制那份树拷贝实现。
/// 同名冲突**不覆盖**：覆盖一个 300MB 的 mod 无法撤销，而用户往往是想"再加一个版本"；
/// 冲突时整批不写入，避免"导了一半"这种最难收拾的状态。
pub fn import(game_dir: &Path, kind: Kind, sources: &[PathBuf]) -> Result<Vec<String>, Error> {
    let dest_dir = dir(game_dir, kind);
    std::fs::create_dir_all(&dest_dir)?;
    if sources.is_empty() {
        return Err(Error::Invalid("no files to import".into()));
    }
    let mut conflicts = Vec::new();
    let mut planned: Vec<(PathBuf, PathBuf, String, bool)> = Vec::new();
    for src in sources {
        let Some(name) = src.file_name().and_then(|n| n.to_str()) else {
            return Err(Error::Invalid(format!(
                "unusable source path: {}",
                src.display()
            )));
        };
        // 源文件名同样校验：拖进来的东西叫 `..` 时会写到目录外
        validate_file_name(name)?;
        let is_dir = src.is_dir();
        if !is_dir && !src.is_file() {
            return Err(Error::Invalid(format!("not a file: {}", src.display())));
        }
        // 文件夹对存档（世界就是一个目录）与资源包（可以是解压形态）都是正常内容，
        // 但 mods/ 里只加载 jar：放进去一个文件夹，游戏会当它不存在，
        // 用户看到的是"我导入了却没有这个 mod"。所以这里直接说清楚。
        if is_dir && kind == Kind::Mods {
            return Err(Error::Invalid(format!(
                "{name} 是文件夹：模组需要 jar/zip 文件（文件夹放进 mods 不会被游戏加载）"
            )));
        }
        if !is_dir && kind == Kind::Saves && is_archive(name) {
            // 游戏只认 `saves/<世界>/level.dat`，压缩包放进去不会变成存档。
            // 与其留一个删不掉也用不上的条目，不如现在说清楚。
            return Err(Error::Invalid(format!(
                "{name} 是压缩包：存档需要世界文件夹（压缩包导入随整合包支持一起做）"
            )));
        }
        if dest_dir.join(name).exists() {
            conflicts.push(name.to_string());
            continue;
        }
        planned.push((src.clone(), dest_dir.join(name), name.to_string(), is_dir));
    }
    if !conflicts.is_empty() {
        return Err(Error::Invalid(format!(
            "already exists in {}: {} (先删除或改名)",
            kind.dir_name(),
            conflicts.join(", ")
        )));
    }
    let mut done = Vec::new();
    for (src, dest, name, is_dir) in planned {
        if is_dir {
            std::fs::create_dir_all(&dest)?;
            crate::instance::copy_tree(&src, &dest, &mut |_| {})?;
        } else {
            std::fs::copy(&src, &dest)?;
        }
        done.push(name);
    }
    Ok(done)
}

fn is_archive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".zip", ".mrpack", ".rar", ".7z", ".tar", ".gz"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// 删除一个内容文件（启用或禁用位置都找）。
///
/// **幂等**：文件已经不在了就是期望的最终状态，返回 Ok。UI 重试、两个窗口同时删、
/// 用户在文件管理器里先删了——这三种情况都不该报错。
/// 但文件名不合法仍然是错误：那说明调用方在传路径而不是名字。
pub fn remove(game_dir: &Path, kind: Kind, file_name: &str) -> Result<(), Error> {
    validate_file_name(file_name)?;
    let path = match locate(game_dir, kind, file_name) {
        Ok(p) => p,
        Err(Error::NotFound(_)) => return Ok(()),
        Err(e) => return Err(e),
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// 内容目录的绝对路径（UI 的"在文件管理器中打开"用）。目录不存在时创建。
pub fn ensure_dir(game_dir: &Path, kind: Kind) -> Result<PathBuf, Error> {
    let d = dir(game_dir, kind);
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// 造一个假 mod jar：把若干 (条目名, 内容) 打进 zip。
    fn fake_jar(path: &Path, entries: &[(&str, &str)]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn kind_serializes_as_the_minecraft_directory_name() {
        for k in Kind::ALL {
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(json, format!("\"{}\"", k.dir_name()));
            assert_eq!(Kind::parse(k.dir_name()).unwrap(), k);
        }
        assert!(Kind::parse("plugins").is_err());
        assert!(Kind::Mods.supports_enable());
        assert!(
            !Kind::Resourcepacks.supports_enable(),
            "资源包的启用状态由游戏自己管"
        );
    }

    #[test]
    fn file_names_reject_traversal_but_allow_extensions() {
        for bad in [
            "", ".", "..", "../x.jar", "a/b.jar", "a\\b.jar", "x..y", "a\u{0}b",
        ] {
            assert!(validate_file_name(bad).is_err(), "should reject {bad:?}");
        }
        assert!(validate_file_name("jei-1.21.4-fabric-19.8.0.jar").is_ok());
        assert!(validate_file_name("我的材质包.zip").is_ok());
        assert!(validate_file_name(&format!("{}.jar", "x".repeat(190))).is_ok());
        assert!(validate_file_name(&format!("{}.jar", "x".repeat(200))).is_err());
    }

    #[test]
    fn parses_fabric_metadata() {
        let d = tmp();
        let jar = d.path().join("mods/jei.jar");
        fake_jar(
            &jar,
            &[(
                "fabric.mod.json",
                r#"{"schemaVersion":1,"id":"jei","version":"19.8.0","name":"Just Enough Items"}"#,
            )],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.name, "Just Enough Items");
        assert_eq!(meta.id, "jei");
        assert_eq!(meta.version, "19.8.0");
        assert_eq!(meta.loaders, vec!["fabric"]);
    }

    #[test]
    fn parses_forge_toml_and_resolves_the_jar_version_placeholder() {
        let d = tmp();
        let jar = d.path().join("mods/jei-forge.jar");
        // Forge 的 version 常是构建期占位符，真值在 MANIFEST 的 Implementation-Version
        fake_jar(
            &jar,
            &[
                (
                    "META-INF/mods.toml",
                    "modLoader=\"javafml\"\n[[mods]]\nmodId=\"jei\"\nversion=\"${file.jarVersion}\"\ndisplayName=\"Just Enough Items\"\n",
                ),
                (
                    "META-INF/MANIFEST.MF",
                    "Manifest-Version: 1.0\nImplementation-Version: 15.20.0.5\n",
                ),
            ],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.name, "Just Enough Items");
        assert_eq!(meta.id, "jei");
        assert_eq!(meta.version, "15.20.0.5");
        assert_eq!(meta.loaders, vec!["forge"]);
    }

    #[test]
    fn dual_loader_jar_reports_both_loaders_with_fabric_primary() {
        let d = tmp();
        let jar = d.path().join("mods/both.jar");
        fake_jar(
            &jar,
            &[
                (
                    "fabric.mod.json",
                    r#"{"id":"both","version":"1.0","name":"Both Fabric"}"#,
                ),
                (
                    "META-INF/neoforge.mods.toml",
                    "[[mods]]\nmodId=\"both\"\nversion=\"1.0\"\ndisplayName=\"Both Neo\"\n",
                ),
            ],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.name, "Both Fabric");
        assert_eq!(meta.loaders, vec!["fabric", "neoforge"]);
    }

    #[test]
    fn parses_quilt_and_legacy_mcmod_info() {
        let d = tmp();
        let q = d.path().join("mods/quilted.jar");
        fake_jar(
            &q,
            &[(
                "quilt.mod.json",
                r#"{"schema_version":1,"quilt_loader":{"id":"qmod","version":"2.1","metadata":{"name":"Quilted Mod"}}}"#,
            )],
        );
        let m = read_mod_meta(&q).unwrap();
        assert_eq!(
            (m.name.as_str(), m.id.as_str(), m.version.as_str()),
            ("Quilted Mod", "qmod", "2.1")
        );
        assert_eq!(m.loaders, vec!["quilt"]);

        let legacy = d.path().join("mods/old.jar");
        fake_jar(
            &legacy,
            &[(
                "mcmod.info",
                r#"[{"modid":"oldmod","name":"Old Mod","version":"1.2.3"}]"#,
            )],
        );
        let m2 = read_mod_meta(&legacy).unwrap();
        assert_eq!((m2.name.as_str(), m2.id.as_str()), ("Old Mod", "oldmod"));
        assert_eq!(m2.loaders, vec!["legacy"]);
    }

    #[test]
    fn parses_fabric_dependencies_whether_the_constraint_is_a_string_or_a_list() {
        // 真样本（sodium 0.6.13）里两种写法同时出现：`"minecraft": ["1.21.4"]` 是数组，
        // `"fabricloader": ">=0.16.0"` 是字符串。只按一种解析会让整个 fabric.mod.json
        // 解码失败，于是这个 mod 从"有元数据"退化成"读不出名字"。
        let d = tmp();
        let jar = d.path().join("mods/sodium.jar");
        fake_jar(
            &jar,
            &[(
                "fabric.mod.json",
                r#"{
                    "schemaVersion": 1, "id": "sodium", "version": "0.6.13", "name": "Sodium",
                    "depends": {
                        "minecraft": ["1.21.4"],
                        "fabricloader": ">=0.16.0",
                        "fabric-resource-loader-v0": "*",
                        "fabric-rendering-fluids-v1": [">=2.0.0", "<3.0.0"]
                    },
                    "breaks": { "optifabric": "*" }
                }"#,
            )],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.name, "Sodium");
        assert_eq!(
            meta.depends,
            vec![
                // BTreeMap ⇒ 按 modId 稳定排序（HashMap 的话每次扫描顺序都可能不同）
                "fabric-rendering-fluids-v1 >=2.0.0 <3.0.0",
                // `*` 是"任意版本"，写出来只是噪音
                "fabric-resource-loader-v0",
                "fabricloader >=0.16.0",
                "minecraft 1.21.4",
            ],
            "{:?}",
            meta.depends
        );
        // breaks 是"与这些 mod 冲突"，不是依赖，混进来会给出完全相反的建议
        assert!(
            !meta.depends.iter().any(|x| x.contains("optifabric")),
            "{:?}",
            meta.depends
        );
    }

    #[test]
    fn parses_forge_dependencies_and_keeps_only_the_required_ones() {
        let d = tmp();
        let jar = d.path().join("mods/jei.jar");
        // 真样本（JEI 的 neoforge.mods.toml）用 `[[dependencies.<自己的 modId>]]`，
        // 所以依赖不在顶层，得遍历 dependencies 表的所有值
        fake_jar(
            &jar,
            &[(
                "META-INF/neoforge.mods.toml",
                r#"modLoader="javafml"
loaderVersion="[4,)"
[[mods]]
modId="jei"
version="20.0.0.4"
displayName="Just Enough Items"
[[dependencies.jei]]
modId="neoforge"
type="required"
versionRange="[21.4.121,)"
[[dependencies.jei]]
modId="minecraft"
type="required"
versionRange="[1.21.4, 1.21.5)"
[[dependencies.jei]]
modId="some_optional"
type="optional"
versionRange="*"
[[dependencies.jei]]
modId="conflicting_mod"
type="incompatible"
versionRange="*"
"#,
            )],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.id, "jei");
        assert_eq!(meta.loaders, vec!["neoforge"]);
        assert_eq!(
            meta.depends,
            vec!["minecraft [1.21.4, 1.21.5)", "neoforge [21.4.121,)"],
            "{:?}",
            meta.depends
        );
    }

    #[test]
    fn legacy_forge_uses_mandatory_instead_of_type() {
        let d = tmp();
        let jar = d.path().join("mods/oldforge.jar");
        fake_jar(
            &jar,
            &[(
                "META-INF/mods.toml",
                r#"modLoader="javafml"
[[mods]]
modId="oldforge"
version="1.0"
displayName="Old Forge Mod"
[[dependencies.oldforge]]
modId="forge"
mandatory=true
versionRange="[36,)"
[[dependencies.oldforge]]
modId="not_needed"
mandatory=false
versionRange="*"
"#,
            )],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.depends, vec!["forge [36,)"], "{:?}", meta.depends);
    }

    #[test]
    fn quilt_dependencies_come_from_the_loader_section() {
        let d = tmp();
        let jar = d.path().join("mods/quilted.jar");
        fake_jar(
            &jar,
            &[(
                "quilt.mod.json",
                r#"{
                    "schema_version": 1,
                    "quilt_loader": {
                        "id": "qmod", "version": "2.1",
                        "metadata": { "name": "Quilted Mod" },
                        "depends": [
                            { "id": "minecraft", "versions": "1.20.1" },
                            { "id": "qsl" },
                            "fabric-api",
                            { "id": "maybe", "versions": "*", "optional": true }
                        ]
                    }
                }"#,
            )],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(
            meta.depends,
            vec!["fabric-api", "minecraft 1.20.1", "qsl"],
            "{:?}",
            meta.depends
        );
    }

    #[test]
    fn dependency_lists_are_bounded() {
        // 元数据是别人写的：不设上限就等于让别人的 jar 决定我们往 UI 塞多少行
        let d = tmp();
        let jar = d.path().join("mods/many.jar");
        let depends: Vec<String> = (0..500).map(|i| format!("\"dep{i}\": \"*\"")).collect();
        fake_jar(
            &jar,
            &[(
                "fabric.mod.json",
                &format!(
                    "{{\"id\":\"many\",\"version\":\"1\",\"name\":\"Many\",\"depends\":{{{}}}}}",
                    depends.join(",")
                ),
            )],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.depends.len(), MAX_DEPENDS, "{:?}", meta.depends.len());
    }

    #[test]
    fn metadata_failures_degrade_to_the_file_name() {
        let d = tmp();
        // 不是 zip
        let txt = d.path().join("mods/notajar.jar");
        std::fs::create_dir_all(txt.parent().unwrap()).unwrap();
        std::fs::write(&txt, b"plain text").unwrap();
        assert_eq!(read_mod_meta(&txt), None);
        // 是 zip 但没有任何元数据条目
        let bare = d.path().join("mods/bare.jar");
        fake_jar(&bare, &[("SomeClass.class", "cafe babe")]);
        assert_eq!(read_mod_meta(&bare), None);
        // 元数据存在但缺 id → 不认（显示文件名比显示一个空 id 有用）
        let noid = d.path().join("mods/noid.jar");
        fake_jar(&noid, &[("fabric.mod.json", r#"{"version":"1.0"}"#)]);
        assert_eq!(read_mod_meta(&noid), None);
        // 扫描时它们仍然出现在列表里，只是名字来自文件名
        let items = scan(d.path(), Kind::Mods);
        assert_eq!(items.len(), 3);
        assert!(items.iter().any(|i| i.display_name == "notajar"));
        assert!(items.iter().any(|i| i.display_name == "bare"));
    }

    #[test]
    fn huge_metadata_entries_are_not_read() {
        let d = tmp();
        let jar = d.path().join("mods/huge.jar");
        let big = "x".repeat((METADATA_LIMIT + 1) as usize);
        fake_jar(&jar, &[("fabric.mod.json", &big)]);
        // 超大条目被跳过（不是解压失败崩溃，也不是把 512KB 读进内存）
        assert_eq!(read_mod_meta(&jar), None);
    }

    #[test]
    fn scan_reports_enabled_and_disabled_mods() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        fake_jar(
            &game.join("mods/on.jar"),
            &[(
                "fabric.mod.json",
                r#"{"id":"on","version":"1","name":"On Mod"}"#,
            )],
        );
        fake_jar(
            &game.join("mods/disabled/off.jar"),
            &[(
                "fabric.mod.json",
                r#"{"id":"off","version":"1","name":"Off Mod"}"#,
            )],
        );
        // `.disabled` 后缀是同一种禁用态的另一种写法
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/suffix.jar.disabled"), b"fake").unwrap();
        // 隐藏文件、临时文件与下载器的续传半成品都不是内容；子目录本身也不该出现在列表里
        std::fs::write(game.join("mods/.DS_Store"), b"x").unwrap();
        std::fs::write(game.join("mods/part.tmp"), b"x").unwrap();
        // 下载中断会留下 .part；把它显示成一个"mod"，用户会以为装了个坏 mod
        std::fs::write(game.join("mods/half.jar.part"), b"partial").unwrap();

        let items = scan(&game, Kind::Mods);
        let names: Vec<_> = items
            .iter()
            .map(|i| (i.file_name.clone(), i.enabled))
            .collect();
        assert_eq!(
            names,
            vec![
                ("on.jar".to_string(), true),
                ("off.jar".to_string(), false),
                ("suffix.jar.disabled".to_string(), false),
            ],
            "{names:?}"
        );
        let s = stats(&items, Kind::Mods);
        assert_eq!((s.total, s.enabled), (3, 1));
        assert!(s.bytes > 0);
    }

    #[test]
    fn missing_content_directory_is_an_empty_list_not_an_error() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        std::fs::create_dir_all(&game).unwrap();
        for k in Kind::ALL {
            assert!(scan(&game, k).is_empty());
        }
    }

    #[test]
    fn set_enabled_moves_between_mods_and_disabled() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        fake_jar(
            &game.join("mods/x.jar"),
            &[("fabric.mod.json", r#"{"id":"x","version":"1","name":"X"}"#)],
        );

        let off = set_enabled(&game, "x.jar", false).unwrap();
        assert!(!off.enabled);
        assert_eq!(off.file_name, "x.jar");
        assert!(!game.join("mods/x.jar").exists());
        assert!(game.join("mods/disabled/x.jar").is_file());
        // 元数据在禁用态下依然解析（列表里要能看出这是什么 mod）
        assert_eq!(off.display_name, "X");

        let on = set_enabled(&game, "x.jar", true).unwrap();
        assert!(on.enabled);
        assert!(game.join("mods/x.jar").is_file());
        assert!(!game.join("mods/disabled/x.jar").exists());

        // 幂等：连点两次不该失败
        set_enabled(&game, "x.jar", true).unwrap();
        assert!(game.join("mods/x.jar").is_file());
    }

    #[test]
    fn set_enabled_handles_the_suffix_convention() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/y.jar.disabled"), b"fake").unwrap();

        let on = set_enabled(&game, "y.jar.disabled", true).unwrap();
        assert!(on.enabled);
        assert_eq!(on.file_name, "y.jar", "启用时去掉 .disabled 后缀");
        assert!(game.join("mods/y.jar").is_file());
        // 用新名字也能再禁用回去
        let off = set_enabled(&game, "y.jar", false).unwrap();
        assert!(!off.enabled);
        assert!(game.join("mods/disabled/y.jar").is_file());
    }

    #[test]
    fn set_enabled_refuses_to_overwrite_an_existing_file() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        std::fs::create_dir_all(game.join("mods/disabled")).unwrap();
        std::fs::write(game.join("mods/dup.jar"), b"active").unwrap();
        std::fs::write(game.join("mods/disabled/dup.jar"), b"disabled").unwrap();
        let err = set_enabled(&game, "dup.jar", false).unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"), "{err}");
        // 两个文件都还在：失败不能留下半截状态
        assert_eq!(std::fs::read(game.join("mods/dup.jar")).unwrap(), b"active");
        assert_eq!(
            std::fs::read(game.join("mods/disabled/dup.jar")).unwrap(),
            b"disabled"
        );
    }

    #[test]
    fn content_operations_reject_traversal_in_file_names() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(d.path().join("secret.txt"), b"secret").unwrap();

        // 文件名参数里出现分隔符或 .. 一律拒绝：内容操作只接受实例目录内的裸文件名
        for name in [
            "../secret.txt",
            "..\\secret.txt",
            "a/b.jar",
            "a\\b.jar",
            ".",
            "..",
        ] {
            assert!(remove(&game, Kind::Mods, name).is_err(), "remove {name:?}");
            assert!(
                set_enabled(&game, name, false).is_err(),
                "set_enabled {name:?}"
            );
        }
        // 源路径本身的文件名不合法时也拒绝（拖进来的东西叫 `..` 会写到目录外）
        for name in ["..", "../.."] {
            assert!(
                import(
                    &game,
                    Kind::Mods,
                    std::slice::from_ref(&d.path().join(name))
                )
                .is_err(),
                "import {name:?}"
            );
        }
        // 目录外的文件没被碰过
        assert_eq!(
            std::fs::read_to_string(d.path().join("secret.txt")).unwrap(),
            "secret"
        );
    }

    #[test]
    fn import_from_anywhere_always_writes_inside_the_content_dir() {
        // 源路径**可以**在磁盘任何位置：那是原生文件选择器给的、用户显式选的，
        // 要求"源必须在数据目录内"等于让导入功能没法用。
        // 真正的保证是写入点：永远落在 `<game_dir>/<kind>/` 里，且用源的裸文件名。
        // 所以哪怕源写成 `../../secret.txt`，结果也只会是 `mods/secret.txt`。
        let d = tmp();
        let game = d.path().join(".minecraft");
        let outside = d.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        // mods/ 要先存在：下面的穿越路径靠文件系统解析 `..`，中间目录不在就 ENOENT
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(outside.join("pack.zip"), b"zip").unwrap();
        std::fs::write(d.path().join("secret.txt"), b"secret").unwrap();

        let done = import(
            &game,
            Kind::Resourcepacks,
            std::slice::from_ref(&outside.join("pack.zip")),
        )
        .unwrap();
        assert_eq!(done, vec!["pack.zip"]);
        assert_eq!(
            std::fs::read(game.join("resourcepacks/pack.zip")).unwrap(),
            b"zip"
        );
        // 导入是复制不是移动
        assert!(outside.join("pack.zip").is_file());

        // 穿越形式的源路径：file_name() 是裸名，所以写进 mods/，不会跑出内容目录。
        // 注意这条路径要穿过**真实存在**的目录：`..` 是靠文件系统解析的，
        // 中间目录不存在时 stat 直接 ENOENT（这一点的平台差异见 CI 记录）。
        let traversing = game.join("mods/../../secret.txt");
        assert!(traversing.is_file(), "样本路径本身要能解析到真文件");
        let done = import(&game, Kind::Mods, std::slice::from_ref(&traversing)).unwrap();
        assert_eq!(done, vec!["secret.txt"]);
        assert_eq!(
            std::fs::read(game.join("mods/secret.txt")).unwrap(),
            b"secret"
        );
        // 源文件没被移动或删除
        assert_eq!(
            std::fs::read_to_string(d.path().join("secret.txt")).unwrap(),
            "secret"
        );
        // 写入点没有跑到内容目录之外：游戏目录里除了 mods/ 与 resourcepacks/ 不该多出东西
        let mut top: Vec<String> = std::fs::read_dir(&game)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        top.sort();
        assert_eq!(top, vec!["mods", "resourcepacks"], "{top:?}");
        let mut in_mods: Vec<String> = std::fs::read_dir(game.join("mods"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        in_mods.sort();
        assert_eq!(in_mods, vec!["secret.txt"], "{in_mods:?}");
    }

    #[test]
    fn import_copies_and_refuses_to_overwrite() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        let src = d.path().join("downloads");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.jar"), b"aaa").unwrap();
        std::fs::write(src.join("b.zip"), b"bbb").unwrap();

        let done = import(&game, Kind::Resourcepacks, &[src.join("b.zip")]).unwrap();
        assert_eq!(done, vec!["b.zip"]);
        assert_eq!(
            std::fs::read(game.join("resourcepacks/b.zip")).unwrap(),
            b"bbb"
        );

        // 同名冲突：整批拒绝，而不是覆盖一半
        std::fs::write(src.join("b2.zip"), b"new").unwrap();
        std::fs::rename(src.join("b2.zip"), src.join("b.zip")).unwrap_or(());
        let err = import(
            &game,
            Kind::Resourcepacks,
            &[src.join("a.jar"), src.join("b.zip")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert!(
            !game.join("resourcepacks/a.jar").exists(),
            "冲突时整批不写入"
        );

        // 空列表是错误而不是静默成功（UI 传了个空数组说明它自己出问题了）
        assert!(import(&game, Kind::Mods, &[]).is_err());
        // 文件夹不是 mod（mods/ 里只加载 jar），说清楚比留一个不生效的条目好
        let err = import(&game, Kind::Mods, std::slice::from_ref(&src)).unwrap_err();
        assert!(err.to_string().contains("文件夹"), "{err}");
        // 同一个文件夹对资源包是合法形态（解压形态的资源包就是一个目录）
        assert_eq!(
            import(&game, Kind::Resourcepacks, std::slice::from_ref(&src)).unwrap(),
            vec!["downloads"]
        );
        assert!(game.join("resourcepacks/downloads/a.jar").is_file());
    }

    #[test]
    fn remove_finds_files_in_both_locations_and_is_idempotent() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        fake_jar(&game.join("mods/a.jar"), &[]);
        fake_jar(&game.join("mods/disabled/b.jar"), &[]);
        remove(&game, Kind::Mods, "a.jar").unwrap();
        remove(&game, Kind::Mods, "b.jar").unwrap();
        assert!(!game.join("mods/a.jar").exists());
        assert!(!game.join("mods/disabled/b.jar").exists());
        // 删两次不报错；删不存在的文件也不报错
        remove(&game, Kind::Mods, "a.jar").unwrap();
        remove(&game, Kind::Mods, "never-existed.jar").unwrap();
        // 找不到的错误要能读出来（locate 对存在性敏感）
        assert!(locate(&game, Kind::Mods, "nope.jar").is_err());
    }

    #[test]
    fn import_accepts_a_world_folder_but_not_an_archive_into_saves() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        let world = d.path().join("downloads/新世界");
        std::fs::create_dir_all(world.join("region")).unwrap();
        std::fs::write(world.join("level.dat"), b"level").unwrap();
        std::fs::write(world.join("region/r.0.0.mca"), b"chunk").unwrap();

        let done = import(&game, Kind::Saves, std::slice::from_ref(&world)).unwrap();
        assert_eq!(done, vec!["新世界"]);
        assert_eq!(
            std::fs::read(game.join("saves/新世界/region/r.0.0.mca")).unwrap(),
            b"chunk",
            "世界要连子目录一起搬过来"
        );

        // 压缩包放进 saves/ 不会变成存档（游戏只认 saves/<世界>/level.dat），
        // 所以直接拒绝，而不是留一个用不上的条目让用户去猜
        let zip = d.path().join("downloads/world.zip");
        std::fs::write(&zip, b"PK").unwrap();
        let err = import(&game, Kind::Saves, std::slice::from_ref(&zip)).unwrap_err();
        assert!(err.to_string().contains("世界文件夹"), "{err}");
        // 同一个压缩包导入资源包是正常的（资源包本来就是 zip）
        assert_eq!(
            import(&game, Kind::Resourcepacks, std::slice::from_ref(&zip)).unwrap(),
            vec!["world.zip"]
        );
    }

    #[test]
    fn ensure_dir_creates_the_content_directory() {
        let d = tmp();
        let game = d.path().join(".minecraft");
        let p = ensure_dir(&game, Kind::Shaderpacks).unwrap();
        assert!(p.is_dir());
        assert!(p.ends_with("shaderpacks"));
    }
}
