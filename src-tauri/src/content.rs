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

/// 依赖类型。取值与 Hoard 的 `version_dependencies.dependency_type` **完全一致**
/// （`required` / `optional` / `incompatible` / `embedded`），也是 Modrinth 的那一套——
/// 计划里要求"不自己发明第二套依赖模型"，所以这里不新增第五种，也不改名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepKind {
    Required,
    /// 缺了也能跑（fabric 的 `recommends`/`suggests`、toml 的 `optional`/`discouraged`）
    Optional,
    /// **装了才起不来**（fabric 的 `breaks`/`conflicts`、toml 的 `incompatible`）
    Incompatible,
    /// 已经被内嵌提供（fabric/quilt 的 `jars` 嵌套 jar）
    Embedded,
}

impl DepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DepKind::Required => "required",
            DepKind::Optional => "optional",
            DepKind::Incompatible => "incompatible",
            DepKind::Embedded => "embedded",
        }
    }
}

/// 一条依赖。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModDep {
    pub id: String,
    pub kind: DepKind,
    /// 版本约束**原文**（`>=0.11.3`、`<0.6.6`、`*`、`[1.21,)`…）。
    /// 我们不求值它，理由见 `check_dependencies`。
    pub constraint: String,
}

/// jar 内的 mod 元数据。`loader` 可能多个（同一个 jar 同时发 Fabric 与 NeoForge 版）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModMeta {
    pub name: String,
    pub id: String,
    pub version: String,
    pub loaders: Vec<String>,
    /// 这个 jar 还顶替哪些 mod id（fabric/quilt 的 `provides`）。
    /// 依赖检查必须算上它们，否则会报一堆假缺失：实测 Fabulously Optimized 里
    /// 7 个 mod 依赖 `fabric`，而装的是 `fabric-api`（它 `provides: ["fabric"]`）。
    #[serde(default)]
    pub provides: Vec<String>,
    /// 全部依赖，按类型分开（不是一条拍平的字符串列表）。
    ///
    /// 曾经这里只有"必需依赖的展示字符串"，注释写着"把 optional / incompatible
    /// 也列进来，用户会以为缺了这个就起不来"。那个顾虑是对的，而解法是**分类**
    /// 而不是**只留一类**：W11 要靠 `incompatible` 拦住冲突、靠 `embedded` 认出
    /// 内嵌提供，少了任何一类检查就会给出错误结论。
    pub deps: Vec<ModDep>,
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
    /// 实测 46 个真 jar 里 10 个有 `recommends`、9 个有 `suggests`
    #[serde(default)]
    recommends: std::collections::BTreeMap<String, DepValue>,
    #[serde(default)]
    suggests: std::collections::BTreeMap<String, DepValue>,
    /// 实测 21/46 有 `breaks`、4/46 有 `conflicts`。
    /// 两者的差别对我们很重要：`breaks` 实测**带版本约束**（`{"sodium":"<0.6.6"}`），
    /// `conflicts` 实测是**无条件**的（`{"optifabric":"*"}`）——见 `check_dependencies`
    /// 里"只在无条件时阻止"的理由。
    #[serde(default)]
    breaks: std::collections::BTreeMap<String, DepValue>,
    #[serde(default)]
    conflicts: std::collections::BTreeMap<String, DepValue>,
    /// 嵌套 jar（`[{"file":"META-INF/jars/x.jar"}]`）。实测 20/46 有，共 109 个，
    /// 其中 2 个自己还带 `jars`，所以要递归（深度见 `NESTED_DEPTH`）。
    #[serde(default)]
    jars: NestedJars,
    /// 这个 jar 还顶替哪些 mod id。实测 4/46 有：`fabric-api provides ["fabric"]`、
    /// `sodium provides ["indium"]`、`cloth-config provides ["cloth-config2"]`、
    /// `e4mc provides ["e4mc_minecraft"]`。
    /// 少了它，依赖检查会在一个完全正常的整合包上报一堆"缺 fabric"。
    #[serde(default)]
    provides: Vec<ProvidesEntry>,
}

/// `provides` 的条目实测是裸字符串，但规范也允许 `{"id": "..."}` 对象。
/// 两种都吃：只按一种解析的话，遇到另一种会让**整个** fabric.mod.json 解码失败，
/// 于是这个 mod 从"有元数据"退化成"读不出名字"（同 `DepValue` 的理由）。
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ProvidesEntry {
    Id(String),
    Full {
        #[serde(default)]
        id: String,
    },
}

impl ProvidesEntry {
    fn id(&self) -> &str {
        match self {
            ProvidesEntry::Id(s) => s,
            ProvidesEntry::Full { id } => id,
        }
    }
}

/// `jars` 实测是数组，但单个元素时也有写成对象的（各家打包工具不一致），两种都吃。
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum NestedJars {
    #[default]
    None,
    One(NestedJar),
    Many(Vec<NestedJar>),
}

#[derive(Debug, Deserialize)]
struct NestedJar {
    #[serde(default)]
    file: String,
}

impl NestedJars {
    fn paths(&self) -> Vec<String> {
        match self {
            NestedJars::None => Vec::new(),
            NestedJars::One(j) => vec![j.file.clone()],
            NestedJars::Many(v) => v.iter().map(|j| j.file.clone()).collect(),
        }
    }
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
    #[serde(default)]
    breaks: Vec<QuiltDep>,
    #[serde(default)]
    jars: Vec<QuiltJar>,
    /// quilt 也有 `provides`，与 fabric 同义
    #[serde(default)]
    provides: Vec<ProvidesEntry>,
}

#[derive(Deserialize)]
struct QuiltJar {
    #[serde(default)]
    file: String,
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
    /// NeoForge 用 `type`（required / optional / incompatible / discouraged），
    /// Forge 1.13+ 用 `mandatory`。两套都映射到同一组 `DepKind`，
    /// 这样上层只有一种依赖模型（与 Hoard 的 dependency_type 一致）。
    fn kind(&self) -> DepKind {
        if !self.dep_type.is_empty() {
            return match self.dep_type.as_str() {
                "required" => DepKind::Required,
                "incompatible" => DepKind::Incompatible,
                // optional 与 discouraged 都是"缺了能跑"，区别只是语气
                _ => DepKind::Optional,
            };
        }
        if self.mandatory == Some(false) {
            DepKind::Optional
        } else {
            DepKind::Required
        }
    }
}

/// quilt 的依赖项两种写法（裸 id / 带字段对象）统一成 `ModDep`。
/// `optional` 为真时用 `optional_kind`，否则用 `required_kind`——
/// `breaks` 段传两次 Incompatible，因为"不兼容"没有可选一说。
fn quilt_dep(d: QuiltDep, required_kind: DepKind, optional_kind: DepKind) -> ModDep {
    match d {
        QuiltDep::Id(id) => ModDep {
            id,
            kind: required_kind,
            constraint: String::new(),
        },
        QuiltDep::Full {
            id,
            versions,
            optional,
        } => ModDep {
            id,
            kind: if optional {
                optional_kind
            } else {
                required_kind
            },
            constraint: versions.map(|v| v.constraint()).unwrap_or_default(),
        },
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

/// 依赖项统一排序、去重、截断。上限 `MAX_DEPENDS`——元数据是别人写的，
/// 不设上限等于让别人的文件决定我们往 UI 塞多少行。
///
/// **排序只在这一处**：fabric 的 depends 是对象（天然按 key 有序），
/// toml 与 quilt 是数组（声明顺序）。不统一的话同一个 mod 的 Fabric 版与
/// NeoForge 版会给出不同顺序，看起来像列表在乱跳。
/// 去重按（id, kind）：同一个 id 同时是 required 与 incompatible 是包作者的错，
/// 但两条都留着比悄悄丢一条好——丢掉的那条正好是要拦住冲突的那条。
fn deps_from(items: Vec<ModDep>) -> Vec<ModDep> {
    let mut out: Vec<ModDep> = items
        .into_iter()
        .map(|mut d| {
            d.id = d.id.trim().to_string();
            d.constraint = d.constraint.trim().to_string();
            d
        })
        .filter(|d| !d.id.is_empty())
        .collect();
    out.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then(a.kind.cmp(&b.kind))
            // 约束也进排序键：同一个 (id, kind) 出现两次时（元数据写重了）
            // 顺序才不由输入顺序决定。测试要能对着"排好序的三元组"断言，
            // 就得让生产代码的排序键与它一致——两边各写一份比较器，
            // 验的是各自那份副本。
            .then(a.constraint.cmp(&b.constraint))
    });
    out.dedup_by(|a, b| a.id == b.id && a.kind == b.kind);
    out.truncate(MAX_DEPENDS);
    out
}

/// `provides` 统一成去重排序的 id 列表，同样受 `MAX_DEPENDS` 约束。
fn provides_from(entries: Vec<ProvidesEntry>) -> Vec<String> {
    let mut out: Vec<String> = entries
        .into_iter()
        .map(|e| e.id().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out.truncate(MAX_DEPENDS);
    out
}

/// 把一个 `modId -> 约束` 的表转成某一类依赖。
fn deps_of(
    map: std::collections::BTreeMap<String, DepValue>,
    kind: DepKind,
) -> impl Iterator<Item = ModDep> {
    map.into_iter().map(move |(id, v)| ModDep {
        id,
        kind,
        constraint: v.constraint(),
    })
}

/// 嵌套 jar 的读取上限与递归深度。
///
/// 深度 2 不是拍的：实测 Fabulously Optimized 1.21.4 的 46 个 mod 里有 109 个嵌套 jar，
/// 其中 2 个自己还带 `jars`（controlify 内嵌的 yet-another-config-lib、
/// lambdynamiclights 内嵌的 lambdynamiclights-api）。只读一层会漏掉那两个提供的 id。
/// 深度再深就是给别人的文件决定我们递归多久的机会，所以封顶。
const NESTED_JAR_LIMIT: u64 = 32 * 1024 * 1024;
const NESTED_DEPTH: u8 = 2;
/// 一次扫描最多读多少个嵌套 jar。防的是"一个 jar 里塞几千个嵌套 jar"。
const NESTED_COUNT_LIMIT: usize = 256;

/// 读出嵌套 jar 提供的 mod id（`Embedded` 依赖）。
///
/// **这一步是正确性要求，不是锦上添花**：同一个实测样本里有 **15 个依赖 id
/// 只由嵌套 jar 提供**（fabric-api 的各个子模块、sodium 内嵌的
/// fabric-renderer-api-v1、controlify 内嵌的 yet_another_config_lib_v3…）。
/// 只扫顶层 jar 就会在一个完全正常的整合包上报 15 个假"缺依赖"，
/// 那比不做检查更糟——用户会学会忽略所有警告。
fn nested_ids<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    entries: &[String],
    depth: u8,
    budget: &mut usize,
) -> Vec<ModDep> {
    if depth == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for name in entries {
        if *budget == 0 {
            break;
        }
        let Ok(mut entry) = archive.by_name(name) else {
            continue;
        };
        // 嵌套 jar 的大小由别人的文件决定，读进内存之前先看声明的大小
        if entry.size() > NESTED_JAR_LIMIT {
            continue;
        }
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_err() || bytes.len() as u64 > NESTED_JAR_LIMIT {
            continue;
        }
        *budget -= 1;
        let Ok(mut inner) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
            continue;
        };
        // 嵌套 jar 用的元数据格式与外层一样，所以复用同一套解析
        let text = read_entry(&mut inner, "fabric.mod.json")
            .or_else(|| read_entry(&mut inner, "quilt.mod.json"));
        let Some(text) = text else { continue };
        let id = parse_json_lenient::<FabricModJson>(&text)
            .map(|f| (f.id, f.jars.paths()))
            .or_else(|| {
                parse_json_lenient::<QuiltModJson>(&text)
                    .and_then(|q| q.quilt_loader)
                    .map(|l| (l.id, l.jars.into_iter().map(|j| j.file).collect()))
            });
        let Some((id, deeper)) = id else { continue };
        if !id.trim().is_empty() {
            out.push(ModDep {
                id: id.trim().to_string(),
                kind: DepKind::Embedded,
                constraint: String::new(),
            });
        }
        out.extend(nested_ids(&mut inner, &deeper, depth - 1, budget));
    }
    out
}

fn nested_budget() -> usize {
    NESTED_COUNT_LIMIT
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
    // 一个 jar 能让我们读多少嵌套 jar，总额封顶（双端 jar 会各扫一次）
    let mut budget = nested_budget();
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
            "fabric.mod.json" => parse_json_lenient::<FabricModJson>(&text).map(|f| {
                let FabricModJson {
                    id,
                    name,
                    version,
                    depends,
                    recommends,
                    suggests,
                    breaks,
                    conflicts,
                    jars,
                    provides,
                } = f;
                let display = name.unwrap_or_else(|| id.clone());
                // 五个段各归一类：depends 必需，recommends/suggests 可选，
                // breaks/conflicts 不兼容。约束原文保留但不求值（见 check_dependencies）
                let mut deps: Vec<ModDep> = deps_of(depends, DepKind::Required)
                    .chain(deps_of(recommends, DepKind::Optional))
                    .chain(deps_of(suggests, DepKind::Optional))
                    .chain(deps_of(breaks, DepKind::Incompatible))
                    .chain(deps_of(conflicts, DepKind::Incompatible))
                    .collect();
                let jar_paths = jars.paths();
                deps.extend(nested_ids(
                    &mut archive,
                    &jar_paths,
                    NESTED_DEPTH,
                    &mut budget,
                ));
                ModMeta {
                    name: display,
                    id,
                    version: version.unwrap_or_default(),
                    loaders: Vec::new(),
                    provides: provides_from(provides),
                    deps: deps_from(deps),
                }
            }),
            "quilt.mod.json" => parse_json_lenient::<QuiltModJson>(&text)
                .and_then(|q| q.quilt_loader)
                .map(|l| {
                    let QuiltLoader {
                        id,
                        version,
                        metadata,
                        depends,
                        breaks,
                        jars,
                        provides,
                    } = l;
                    let display = metadata
                        .as_ref()
                        .and_then(|m| m.name.clone())
                        .unwrap_or_else(|| id.clone());
                    let mut deps: Vec<ModDep> =
                        depends
                            .into_iter()
                            .map(|d| quilt_dep(d, DepKind::Required, DepKind::Optional))
                            .chain(breaks.into_iter().map(|d| {
                                quilt_dep(d, DepKind::Incompatible, DepKind::Incompatible)
                            }))
                            .collect();
                    let jar_paths: Vec<String> = jars.into_iter().map(|j| j.file).collect();
                    deps.extend(nested_ids(
                        &mut archive,
                        &jar_paths,
                        NESTED_DEPTH,
                        &mut budget,
                    ));
                    ModMeta {
                        name: display,
                        id,
                        version: version.unwrap_or_default(),
                        loaders: Vec::new(),
                        provides: provides_from(provides),
                        deps: deps_from(deps),
                    }
                }),
            "mcmod.info" => parse_json_lenient::<Vec<McModInfo>>(&text)
                .and_then(|v| v.into_iter().find(|m| !m.modid.is_empty()))
                .map(|m| ModMeta {
                    name: m.name.unwrap_or_else(|| m.modid.clone()),
                    id: m.modid,
                    version: m.version.unwrap_or_default(),
                    loaders: Vec::new(),
                    // mcmod.info 没有 provides 这个概念
                    provides: Vec::new(),
                    // mcmod.info 的 dependencies 只有 modId，没有版本约束，
                    // 也表达不了"不兼容"，所以只有 required 一类
                    deps: deps_from(
                        m.dependencies
                            .into_iter()
                            .map(|id| ModDep {
                                id,
                                kind: DepKind::Required,
                                constraint: String::new(),
                            })
                            .collect(),
                    ),
                }),
            _ => text
                .parse::<toml::Table>()
                .ok()
                .and_then(|t| t.try_into::<ModsToml>().ok())
                .and_then(|t| {
                    // 依赖挂在 `dependencies.<本 mod 的 modId>` 下，可能有多个键
                    // （多 mod jar），所以把所有值都收进来，而不是只取自己那个
                    let deps: Vec<ModDep> = t
                        .dependencies
                        .values()
                        .flatten()
                        .filter(|d| !d.mod_id.is_empty())
                        .map(|d| ModDep {
                            id: d.mod_id.clone(),
                            kind: d.kind(),
                            constraint: d.version_range.clone(),
                        })
                        .collect();
                    let m = t.mods.into_iter().next()?;
                    Some(ModMeta {
                        name: m.display_name.unwrap_or_else(|| m.mod_id.clone()),
                        id: m.mod_id,
                        // Forge 常写 `${file.jarVersion}`，那是构建期占位符，见下面的兜底
                        version: m.version.unwrap_or_default(),
                        loaders: Vec::new(),
                        // mods.toml 没有 provides 这个概念（Forge/NeoForge 靠 JarJar
                        // 提供内嵌库，那是另一套机制，见 NESTED_DEPTH 的说明）
                        provides: Vec::new(),
                        deps: deps_from(deps),
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

/// 严格 JSON 解析失败后的宽容重试：把字符串字面量里的裸控制字符转义掉。
///
/// 实测 Fabulously Optimized 1.21.4 的 46 个 mod 里有 **3 个**的 fabric.mod.json
/// 是技术上的非法 JSON：description 里有裸换行（Debugify）、字符串里有裸 TAB
/// （mixintrace、NoChatReports）。Fabric Loader 用 Gson 的宽松模式，照样加载它们；
/// 我们严格解析就把这 3 个判成"读不出元数据"——于是它们在 UI 上没有名字，
/// 在依赖检查里**完全不存在**。后者更要紧：一个装着的 mod 对检查看不见，
/// 等于它声明的冲突与它提供的依赖都不算数（6.5% 的 mod 成了盲区）。
///
/// 只在严格解析失败之后才走这条路：合法 JSON 一个字节都不动。
fn parse_json_lenient<T: serde::de::DeserializeOwned>(text: &str) -> Option<T> {
    if let Ok(v) = serde_json::from_str::<T>(text) {
        return Some(v);
    }
    serde_json::from_str::<T>(&escape_control_chars_in_strings(text)).ok()
}

/// 把 JSON **字符串字面量内部**的裸控制字符（< 0x20）换成转义序列。
/// 字符串外面的换行与缩进是合法的，原样保留。
fn escape_control_chars_in_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if !in_string {
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
            continue;
        }
        if escaped {
            out.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            // `\"` 不是字符串结束：反斜杠后面那个字符要原样带过去
            '\\' => {
                out.push(ch);
                escaped = true;
            }
            '"' => {
                in_string = false;
                out.push(ch);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
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
/// 所以这是"版本号显示为占位符"时的正确兜底。解析规则复用 `archive`（含 72 字节折行的拼接）。
fn manifest_version<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> Option<String> {
    let text = read_entry(archive, "META-INF/MANIFEST.MF")?;
    crate::archive::manifest_attr_from_text(&text, "Implementation-Version")
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

// ---------------------------------------------------------------------------
// 依赖与冲突检查（W11）
// ---------------------------------------------------------------------------

/// 平台自己就提供的 mod id——不需要装任何 mod 就存在，所以缺了它们不算"缺依赖"。
///
/// 注意 `fabric` **不在**这里：它是 Fabric API 那个 mod，真的可能没装。
/// 在这里的是 loader 自己（`fabricloader`）与游戏/运行时（`minecraft`、`java`）。
pub fn platform_ids(loader: Option<crate::loader::LoaderKind>) -> Vec<&'static str> {
    let mut out = vec!["minecraft", "java"];
    match loader {
        Some(crate::loader::LoaderKind::Fabric) => out.push("fabricloader"),
        // Quilt Loader 同时提供 quilt_loader 与 fabricloader 两个 id（兼容 Fabric mod）
        Some(crate::loader::LoaderKind::Quilt) => {
            out.push("quilt_loader");
            out.push("fabricloader");
        }
        Some(crate::loader::LoaderKind::Forge) => out.push("forge"),
        Some(crate::loader::LoaderKind::Neoforge) => out.push("neoforge"),
        _ => {}
    }
    out
}

/// 约束是不是"无条件"。**这是能不能阻止安装的分界线**：
/// 实测 46 个真 jar 里 `breaks` 全都带版本区间（`{"sodium":"<0.6.6"}`），
/// 而 `conflicts` 是无条件的（`{"optifabric":"*"}`）。
/// 带区间的我们不判定——见 `DependencyReport::uncertain` 的说明。
fn unconditional(constraint: &str) -> bool {
    let c = constraint.trim();
    c.is_empty() || c == "*" || c.eq_ignore_ascii_case("any")
}

/// 缺一个必需依赖。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingDep {
    pub id: String,
    pub constraint: String,
    /// 谁要它（mod id）。用户要能看出"是 X 需要 Y"，而不只是"缺 Y"。
    pub required_by: String,
}

/// 命中一条不兼容声明。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    /// 冲突对象的 mod id
    pub id: String,
    /// 声明里的版本约束原文
    pub constraint: String,
    /// 谁声明的（可能是已装的那个 mod，不是待装的那个）
    pub declared_by: String,
}

/// 依赖检查结果。
///
/// **不评估版本区间**，这是刻意的：区间语法四家各不相同（fabric 的 `>=1.2 <2`、
/// Maven 的 `[1.21.4,1.21.5)`、maven-metadata 风格、还有裸 `1.21.4`），
/// 自己写一个"看起来判定了兼容性"的求值器，判错时的后果是拦住能用的组合
/// 或放过坏的组合，两者都比明说"我不判定版本"更糟。
/// 所以这里只回答两个**不需要求值就能确定**的问题：
/// 1. 这个 id 在不在？（缺依赖 / 冲突对象存在）
/// 2. 这条不兼容声明是不是无条件的？（是 → 可以拦；否 → 只能提醒）
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyReport {
    /// 缺的必需依赖。**不阻止安装**：约束不求值，所以"缺"只在 id 完全不存在时成立，
    /// 而 id 不存在确实意味着跑不起来——但这属于用户该知道并自己决定的事，
    /// UI 给出"去 Hoard 搜这个 id"的动作，不替他拒绝。
    pub missing: Vec<MissingDep>,
    /// 命中的**无条件**不兼容 → 应当阻止安装
    pub blocked: Vec<Conflict>,
    /// 命中了带版本约束的不兼容，而我们不求值区间 → 只警告，说清是"没判定"
    pub uncertain: Vec<Conflict>,
}

impl DependencyReport {
    pub fn blocks(&self) -> bool {
        !self.blocked.is_empty()
    }
}

/// 检查"把这个 mod 装进这堆已装 mod 里"会不会缺依赖或撞冲突。
///
/// 两个方向都要查：冲突常常是**已装的那一方**声明的
/// （实测 sodium 声明 `breaks: {"optifabric": "*"}`，而 optifabric 自己不会声明
/// 与 sodium 冲突）。只查待装 mod 的声明，就会在安装 optifabric 时放行，
/// 然后游戏启动时炸——那正是这个检查存在的理由。
///
/// `installed` 传全量扫描结果即可，**禁用的项在这里被排除**：禁用的 mod 不会被加载，
/// 既不能满足依赖也不构成冲突。放在函数里而不是让调用方过滤，是因为漏过滤的症状
/// 是"我明明禁用了那个 mod 它还说冲突"，那种 bug 很难从调用方看出来。
pub fn check_dependencies(
    installed: &[ContentItem],
    candidate: &ModMeta,
    loader: Option<crate::loader::LoaderKind>,
) -> DependencyReport {
    let mut report = DependencyReport::default();

    // 待装 mod 自己提供的 id：它本身、它 `provides` 的别名、它内嵌的库。
    // 这一份要单独算出来，因为**自己提供的 id 不能当冲突对象**：
    // 一个 mod 声明 breaks 自己（或自己的别名 / 自己内嵌的库）要么是打包错误，
    // 要么是"别单独装这个库"的意思。把它当冲突，这个 mod 就永远装不进去；
    // 真正该拦的是"另一个独立的 jar 提供了同一个 id"。
    let mut self_ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    self_ids.insert(candidate.id.clone());
    self_ids.extend(candidate.provides.iter().cloned());
    for d in &candidate.deps {
        if d.kind == DepKind::Embedded {
            self_ids.insert(d.id.clone());
        }
    }

    // 已装侧提供的 id：自己的 id + `provides` 声明的别名 + 内嵌 jar 提供的 id
    let mut provided: std::collections::BTreeSet<String> = self_ids.clone();
    for item in installed.iter().filter(|i| i.enabled) {
        let Some(meta) = &item.meta else { continue };
        provided.insert(meta.id.clone());
        provided.extend(meta.provides.iter().cloned());
        for d in &meta.deps {
            if d.kind == DepKind::Embedded {
                provided.insert(d.id.clone());
            }
        }
    }
    let mut available = provided.clone();
    for id in platform_ids(loader) {
        available.insert(id.to_string());
    }

    // 1. 待装 mod 的必需依赖
    for d in candidate
        .deps
        .iter()
        .filter(|d| d.kind == DepKind::Required)
    {
        if !available.contains(&d.id) {
            report.missing.push(MissingDep {
                id: d.id.clone(),
                constraint: d.constraint.clone(),
                required_by: candidate.id.clone(),
            });
        }
    }

    // 2. 待装 mod 声明的不兼容（自己提供的 id 不算，见 self_ids 的说明）
    for d in candidate
        .deps
        .iter()
        .filter(|d| d.kind == DepKind::Incompatible && !self_ids.contains(&d.id))
    {
        if !provided.contains(&d.id) {
            continue;
        }
        let c = Conflict {
            id: d.id.clone(),
            constraint: d.constraint.clone(),
            declared_by: candidate.id.clone(),
        };
        if unconditional(&d.constraint) {
            report.blocked.push(c);
        } else {
            report.uncertain.push(c);
        }
    }

    // 3. 已装 mod 声明的、指向待装 mod 的不兼容（反方向）
    for item in installed.iter().filter(|i| i.enabled) {
        let Some(meta) = &item.meta else { continue };
        for d in meta
            .deps
            .iter()
            .filter(|d| d.kind == DepKind::Incompatible && d.id == candidate.id)
        {
            let c = Conflict {
                // 反向声明：冲突对象是待装的那个，声明者是已装的那个
                id: candidate.id.clone(),
                constraint: d.constraint.clone(),
                declared_by: meta.id.clone(),
            };
            if unconditional(&d.constraint) {
                report.blocked.push(c);
            } else {
                report.uncertain.push(c);
            }
        }
    }

    report.missing.sort_by(|a, b| a.id.cmp(&b.id));
    report.blocked.sort_by(|a, b| a.id.cmp(&b.id));
    report.uncertain.sort_by(|a, b| a.id.cmp(&b.id));
    report
}

/// 检查"把这个已落盘的文件算进这个实例"会不会缺依赖或撞冲突。
///
/// 只用于 mods：资源包 / 着色器 / 存档没有依赖模型。
/// 扫描结果里会包含这个文件自己（它已经在目录里了），所以按文件名把它剔出去——
/// 否则它会跟自己做反向冲突检查。
pub fn check_file(
    game_dir: &Path,
    file_name: &str,
    meta: &ModMeta,
    loader: Option<crate::loader::LoaderKind>,
) -> DependencyReport {
    let mut installed = scan(game_dir, Kind::Mods);
    installed.retain(|i| i.file_name != file_name);
    check_dependencies(&installed, meta, loader)
}

/// 整个实例的依赖体检：把每个启用的 mod 轮流当"待装的那个"，汇总所有问题。
///
/// 这是 UI 上"这个实例现在缺什么"的答案，与安装时那次检查互补：
/// 安装时只能看到"新装的这个与已有的"，而整合包一次装几十个、或者用户手动
/// 往目录里丢文件，都需要一次全局体检才看得出问题。
pub fn check_instance(
    game_dir: &Path,
    loader: Option<crate::loader::LoaderKind>,
) -> DependencyReport {
    let items = scan(game_dir, Kind::Mods);
    let metas: Vec<(String, ModMeta)> = items
        .iter()
        .filter(|i| i.enabled)
        .filter_map(|i| i.meta.clone().map(|m| (i.file_name.clone(), m)))
        .collect();
    let mut report = DependencyReport::default();
    for (file_name, meta) in &metas {
        let mut others = items.clone();
        others.retain(|i| &i.file_name != file_name);
        let r = check_dependencies(&others, meta, loader);
        report.missing.extend(r.missing);
        report.blocked.extend(r.blocked);
        report.uncertain.extend(r.uncertain);
    }
    // 同一对冲突会被两侧各报一次（A 声明 breaks B，B 也声明 breaks A 时），
    // 去重按（声明方, 对象, 约束）
    report
        .blocked
        .sort_by(|a, b| a.declared_by.cmp(&b.declared_by).then(a.id.cmp(&b.id)));
    report
        .blocked
        .dedup_by(|a, b| a.declared_by == b.declared_by && a.id == b.id);
    report
        .uncertain
        .sort_by(|a, b| a.declared_by.cmp(&b.declared_by).then(a.id.cmp(&b.id)));
    report
        .uncertain
        .dedup_by(|a, b| a.declared_by == b.declared_by && a.id == b.id);
    report
        .missing
        .sort_by(|a, b| a.required_by.cmp(&b.required_by).then(a.id.cmp(&b.id)));
    report
        .missing
        .dedup_by(|a, b| a.required_by == b.required_by && a.id == b.id);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// 依赖断言用 (id, kind, constraint) 三元组：直接比 `Vec<ModDep>` 的话，
    /// 失败信息里全是结构体噪音，看不出是哪一项错了。
    fn dep_triples(meta: &ModMeta) -> Vec<(String, DepKind, String)> {
        meta.deps
            .iter()
            .map(|d| (d.id.clone(), d.kind, d.constraint.clone()))
            .collect()
    }

    fn t(id: &str, kind: DepKind, constraint: &str) -> (String, DepKind, String) {
        (id.to_string(), kind, constraint.to_string())
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

    /// 同上，但内容是原始字节：嵌套 jar 本身就是个 zip，不能当文本塞进去。
    fn fake_jar_bytes(path: &Path, entries: &[(&str, &[u8])]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in entries {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body).unwrap();
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
            dep_triples(&meta),
            vec![
                // 按 id 排序（BTreeMap ⇒ 稳定；HashMap 的话每次扫描顺序都可能不同）
                t(
                    "fabric-rendering-fluids-v1",
                    DepKind::Required,
                    ">=2.0.0 <3.0.0"
                ),
                t("fabric-resource-loader-v0", DepKind::Required, "*"),
                t("fabricloader", DepKind::Required, ">=0.16.0"),
                t("minecraft", DepKind::Required, "1.21.4"),
                // breaks 是"与它冲突"，必须单独一类：混进 required 会给出完全相反的建议
                t("optifabric", DepKind::Incompatible, "*"),
            ],
            "{:?}",
            meta.deps
        );
    }

    #[test]
    fn parses_forge_dependencies_into_all_four_kinds() {
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
        // NeoForge 的 `type` 四个取值都要落到对应的 DepKind，
        // 而不是像以前那样只留 required、把其余三类整个丢掉
        assert_eq!(
            dep_triples(&meta),
            vec![
                t("conflicting_mod", DepKind::Incompatible, "*"),
                t("minecraft", DepKind::Required, "[1.21.4, 1.21.5)"),
                t("neoforge", DepKind::Required, "[21.4.121,)"),
                t("some_optional", DepKind::Optional, "*"),
            ],
            "{:?}",
            meta.deps
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
        // Forge 1.13+ 没有 `type` 字段，用 `mandatory`：true → required，false → optional
        assert_eq!(
            dep_triples(&meta),
            vec![
                t("forge", DepKind::Required, "[36,)"),
                t("not_needed", DepKind::Optional, "*"),
            ],
            "{:?}",
            meta.deps
        );
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
        // 四种写法都要认：带 versions 的对象、只有 id 的对象、裸字符串、optional
        assert_eq!(
            dep_triples(&meta),
            vec![
                t("fabric-api", DepKind::Required, ""),
                t("maybe", DepKind::Optional, "*"),
                t("minecraft", DepKind::Required, "1.20.1"),
                t("qsl", DepKind::Required, ""),
            ],
            "{:?}",
            meta.deps
        );
    }

    #[test]
    fn quilt_breaks_become_incompatible_and_jars_become_embedded() {
        let d = tmp();
        let jar = d.path().join("mods/qbreaks.jar");
        // 嵌套 jar 要真的能被读出来：内层是一个自带 fabric.mod.json 的 zip
        let inner = d.path().join("inner.jar");
        fake_jar(
            &inner,
            &[(
                "fabric.mod.json",
                r#"{ "id": "nested_lib", "version": "1.0", "name": "Nested" }"#,
            )],
        );
        let inner_bytes = std::fs::read(&inner).unwrap();
        let index = r#"{
            "schema_version": 1,
            "quilt_loader": {
                "id": "qbreaks", "version": "1.0",
                "depends": [{ "id": "minecraft", "versions": "1.20.1" }],
                "breaks": [{ "id": "conflicting", "versions": "<2.0" }],
                "jars": [{ "file": "META-INF/jars/nested.jar" }]
            }
        }"#;
        fake_jar_bytes(
            &jar,
            &[
                ("quilt.mod.json", index.as_bytes()),
                ("META-INF/jars/nested.jar", inner_bytes.as_slice()),
            ],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.id, "qbreaks");
        assert_eq!(
            dep_triples(&meta),
            vec![
                t("conflicting", DepKind::Incompatible, "<2.0"),
                t("minecraft", DepKind::Required, "1.20.1"),
                // 嵌套 jar 提供的 id 记成 embedded，这样它就不会被算作"缺依赖"
                t("nested_lib", DepKind::Embedded, ""),
            ],
            "{:?}",
            meta.deps
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
        assert_eq!(meta.deps.len(), MAX_DEPENDS, "{:?}", meta.deps.len());
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

    // ---- 依赖与冲突检查 ----

    fn dep(id: &str, kind: DepKind, constraint: &str) -> ModDep {
        ModDep {
            id: id.into(),
            kind,
            constraint: constraint.into(),
        }
    }

    fn meta_of(id: &str, deps: Vec<ModDep>) -> ModMeta {
        ModMeta {
            name: id.into(),
            id: id.into(),
            version: "1.0".into(),
            loaders: vec!["fabric".into()],
            provides: Vec::new(),
            deps,
        }
    }

    fn item_of(meta: ModMeta, enabled: bool) -> ContentItem {
        ContentItem {
            kind: Kind::Mods,
            file_name: format!("{}.jar", meta.id),
            display_name: meta.name.clone(),
            size: 1,
            modified_at: 0,
            enabled,
            meta: Some(meta),
        }
    }

    #[test]
    fn provides_aliases_satisfy_dependencies() {
        // 实测 Fabulously Optimized 里 7 个 mod 依赖 `fabric`，而装的是 `fabric-api`
        // （它声明 `provides: ["fabric"]`）。不认 provides 就会报 7 个假缺失。
        let d = tmp();
        let jar = d.path().join("mods/fabric-api.jar");
        fake_jar(
            &jar,
            &[(
                "fabric.mod.json",
                r#"{ "id": "fabric-api", "version": "0.119.4", "name": "Fabric API",
                    "provides": ["fabric"] }"#,
            )],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(meta.id, "fabric-api");
        assert_eq!(meta.provides, vec!["fabric".to_string()]);

        let installed = [item_of(meta, true)];
        let candidate = meta_of("animatica", vec![dep("fabric", DepKind::Required, "*")]);
        let r = check_dependencies(&installed, &candidate, None);
        assert!(
            r.missing.is_empty(),
            "provides 的别名没被当成已提供：{:?}",
            r.missing
        );
    }

    #[test]
    fn provides_accepts_both_the_string_and_object_forms() {
        // 只按一种写法解析的话，遇到另一种会让**整个** fabric.mod.json 解码失败，
        // 于是这个 mod 从"有元数据"退化成"读不出名字"（同 DepValue 的理由）
        let d = tmp();
        let jar = d.path().join("mods/mixed.jar");
        fake_jar(
            &jar,
            &[(
                "fabric.mod.json",
                r#"{ "id": "mixed", "version": "1.0", "name": "Mixed",
                    "provides": ["bare", { "id": "object-form" }] }"#,
            )],
        );
        let meta = read_mod_meta(&jar).unwrap();
        assert_eq!(
            meta.provides,
            vec!["bare".to_string(), "object-form".to_string()],
            "{:?}",
            meta.provides
        );
    }

    #[test]
    fn lenient_json_recovers_metadata_that_gson_would_accept() {
        // 真样本（Debugify 1.21.4）的 description 里有一个**裸换行**，
        // mixintrace / NoChatReports 的字符串里有裸 TAB。Fabric Loader 用 Gson
        // 宽松模式照样加载，我们严格解析就把它们判成"读不出元数据"——
        // 于是这个装着的 mod 在依赖检查里完全不存在。
        let d = tmp();
        let jar = d.path().join("mods/debugify.jar");
        fake_jar(
            &jar,
            &[(
                "fabric.mod.json",
                "{ \"id\": \"debugify\", \"version\": \"1.21.4+1.1\", \"name\": \"Debugify\",\n  \"description\": \"Fixes bugs found on the tracker\nand the second line\",\n  \"depends\": { \"fabricloader\": \">=0.14.0\", \"minecraft\": \"~1.21.4\" },\n  \"recommends\": { \"modmenu\": \"*\" } }",
            )],
        );
        let meta = read_mod_meta(&jar).expect("裸换行不该让整个元数据报废");
        assert_eq!(meta.id, "debugify");
        assert_eq!(meta.version, "1.21.4+1.1");
        assert_eq!(
            dep_triples(&meta),
            vec![
                t("fabricloader", DepKind::Required, ">=0.14.0"),
                t("minecraft", DepKind::Required, "~1.21.4"),
                t("modmenu", DepKind::Optional, "*"),
            ],
            "{:?}",
            meta.deps
        );

        // 裸 TAB 同样要能恢复
        let jar2 = d.path().join("mods/tabbed.jar");
        fake_jar(
            &jar2,
            &[(
                "fabric.mod.json",
                "{ \"id\": \"tabbed\", \"version\": \"1.0\", \"name\": \"Tab\tbed\" }",
            )],
        );
        let meta2 = read_mod_meta(&jar2).expect("裸 TAB 不该让整个元数据报废");
        assert_eq!(meta2.id, "tabbed");
    }

    #[test]
    fn lenient_pass_leaves_valid_json_alone() {
        // 宽容那一遍只在严格解析失败后才跑，所以合法 JSON 一个字节都不该变。
        // 这条钉的是转义器本身：它不能把字符串外面的换行/缩进也转义掉，
        // 也不能把 `\"` 当成字符串结束。
        let valid = "{\n  \"a\": \"b\\\"c\",\n  \"d\": [\"e\"]\n}";
        assert_eq!(escape_control_chars_in_strings(valid), valid);

        // 字符串里的裸换行要转义，字符串外面那个不要
        let mixed = "{\n\"a\": \"x\ny\"\n}";
        assert_eq!(
            escape_control_chars_in_strings(mixed),
            "{\n\"a\": \"x\\ny\"\n}"
        );
        // 转义后的结果必须能被 serde 接受，否则这一步只是把错误换了个形状
        assert!(
            serde_json::from_str::<serde_json::Value>(&escape_control_chars_in_strings(mixed))
                .is_ok()
        );
    }

    #[test]
    fn instance_check_finds_conflicts_between_installed_mods() {
        // check_dependencies 是纯函数，上面几条测的是它；这条测的是**从磁盘扫上来**
        // 的那一层：scan → 元数据解析 → 汇总。真实路径上还有"禁用的不算"这一条，
        // 只有走完整流程才验得到。
        let d = tmp();
        let game = d.path().join(".minecraft");
        let mods = ensure_dir(&game, Kind::Mods).unwrap();
        fake_jar(
            &mods.join("sodium.jar"),
            &[(
                "fabric.mod.json",
                r#"{ "id": "sodium", "version": "0.6.13", "name": "Sodium",
                    "provides": ["indium"],
                    "breaks": { "optifabric": "*" },
                    "jars": [{ "file": "META-INF/jars/lib.jar" }] }"#,
            )],
        );
        fake_jar(
            &mods.join("optifabric.jar"),
            &[(
                "fabric.mod.json",
                r#"{ "id": "optifabric", "version": "1.0", "name": "OptiFabric" }"#,
            )],
        );
        fake_jar(
            &mods.join("needs-stuff.jar"),
            &[(
                "fabric.mod.json",
                r#"{ "id": "needs_stuff", "version": "1.0", "name": "Needs Stuff",
                    "depends": { "cloth-config": "*", "minecraft": ">=1.21" } }"#,
            )],
        );

        let r = check_instance(&game, Some(crate::loader::LoaderKind::Fabric));
        // sodium 声明的无条件冲突被反向查到（optifabric 自己什么都没声明）
        assert_eq!(r.blocked.len(), 1, "{r:?}");
        assert_eq!(r.blocked[0].declared_by, "sodium");
        assert_eq!(r.blocked[0].id, "optifabric");
        // 缺 cloth-config；minecraft 由平台提供，不该被报出来
        assert_eq!(
            r.missing.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["cloth-config"],
            "{:?}",
            r.missing
        );
        assert_eq!(r.missing[0].required_by, "needs_stuff");

        // 禁用 optifabric 之后冲突就没了：禁用的 mod 不会被加载，
        // 把它算进冲突会让用户以为"我明明关了它还报错"
        let disabled = mods.join("disabled");
        std::fs::create_dir_all(&disabled).unwrap();
        std::fs::rename(mods.join("optifabric.jar"), disabled.join("optifabric.jar")).unwrap();
        let r2 = check_instance(&game, Some(crate::loader::LoaderKind::Fabric));
        assert!(
            r2.blocked.is_empty(),
            "禁用的 mod 还被当成冲突来源：{:?}",
            r2.blocked
        );
        // 缺依赖仍然在（cloth-config 还是没装）
        assert_eq!(r2.missing.len(), 1, "{:?}", r2.missing);
    }

    #[test]
    fn file_check_excludes_the_file_itself() {
        // check_file 用于"刚落盘的这一个"，而它已经在目录里了：
        // 不把自己剔出去，它就会跟自己做反向冲突检查
        let d = tmp();
        let game = d.path().join(".minecraft");
        let mods = ensure_dir(&game, Kind::Mods).unwrap();
        fake_jar(
            &mods.join("selfbreak.jar"),
            &[(
                "fabric.mod.json",
                // 病态但合法的元数据：声明与自己不兼容
                r#"{ "id": "selfbreak", "version": "1.0", "name": "Self",
                    "breaks": { "selfbreak": "*" } }"#,
            )],
        );
        let meta = read_mod_meta(&mods.join("selfbreak.jar")).unwrap();
        let r = check_file(&game, "selfbreak.jar", &meta, None);
        assert!(r.blocked.is_empty(), "自己跟自己冲突了：{:?}", r.blocked);

        // 更真实的两种自指：声明与自己 provides 的别名冲突、与自己内嵌的库冲突
        // （后者的本意通常是"别单独装这个库"，不是"别装我"）
        let d2 = tmp();
        let game2 = d2.path().join(".minecraft");
        let mods2 = ensure_dir(&game2, Kind::Mods).unwrap();
        let inner = d2.path().join("inner.jar");
        fake_jar(
            &inner,
            &[(
                "fabric.mod.json",
                r#"{ "id": "bundled_lib", "version": "1.0", "name": "L" }"#,
            )],
        );
        let inner_bytes = std::fs::read(&inner).unwrap();
        let index = r#"{ "id": "bundler", "version": "1.0", "name": "Bundler",
            "provides": ["bundled_lib"],
            "breaks": { "bundled_lib": "*" },
            "jars": [{ "file": "META-INF/jars/lib.jar" }] }"#;
        fake_jar_bytes(
            &mods2.join("bundler.jar"),
            &[
                ("fabric.mod.json", index.as_bytes()),
                ("META-INF/jars/lib.jar", inner_bytes.as_slice()),
            ],
        );
        let meta2 = read_mod_meta(&mods2.join("bundler.jar")).unwrap();
        assert_eq!(meta2.provides, vec!["bundled_lib".to_string()]);
        let r2 = check_file(&game2, "bundler.jar", &meta2, None);
        assert!(
            r2.blocked.is_empty(),
            "把自己 provides / 内嵌的 id 当成冲突对象了：{:?}",
            r2.blocked
        );
    }

    #[test]
    fn missing_required_dependency_names_who_needs_it() {
        let installed = [item_of(meta_of("sodium", vec![]), true)];
        let candidate = meta_of(
            "iris",
            vec![
                dep("sodium", DepKind::Required, ">=0.6"),
                dep("cloth-config", DepKind::Required, "*"),
            ],
        );
        let r = check_dependencies(&installed, &candidate, None);
        assert_eq!(r.missing.len(), 1, "{:?}", r.missing);
        assert_eq!(r.missing[0].id, "cloth-config");
        assert_eq!(
            r.missing[0].required_by, "iris",
            "用户要能看出是谁需要它，而不只是缺了什么"
        );
        assert!(!r.blocks(), "缺依赖不该被当成冲突拦下来");
    }

    #[test]
    fn a_dependency_provided_by_a_nested_jar_is_not_missing() {
        // 这条是整个检查能不能用的分界线：实测 Fabulously Optimized 1.21.4（46 个 mod）
        // 里有 15 个依赖 id **只由嵌套 jar 提供**。只扫顶层 jar 就会在这个完全正常的
        // 整合包上报 15 个假"缺依赖"，用户会学会忽略所有警告。
        let installed = [item_of(
            meta_of(
                "fabric-api",
                vec![dep("fabric-rendering-v1", DepKind::Embedded, "")],
            ),
            true,
        )];
        let candidate = meta_of(
            "lambdynamiclights",
            vec![dep("fabric-rendering-v1", DepKind::Required, ">=10.0")],
        );
        let r = check_dependencies(&installed, &candidate, None);
        assert!(
            r.missing.is_empty(),
            "内嵌提供的依赖被报成缺失：{:?}",
            r.missing
        );
    }

    #[test]
    fn platform_ids_are_never_reported_missing() {
        // minecraft / java / fabricloader 由游戏与 loader 提供，不是 mod
        let candidate = meta_of(
            "animatica",
            vec![
                dep("minecraft", DepKind::Required, ">=1.21.4"),
                dep("java", DepKind::Required, ">=21"),
                dep("fabricloader", DepKind::Required, ">=0.11.3"),
                dep("fabric", DepKind::Required, "*"),
            ],
        );
        let r = check_dependencies(&[], &candidate, Some(crate::loader::LoaderKind::Fabric));
        // `fabric` 是 Fabric API 那个 mod，真的可能没装——不能跟着一起放过
        assert_eq!(
            r.missing.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["fabric"],
            "{:?}",
            r.missing
        );
    }

    #[test]
    fn unconditional_incompatibility_blocks_in_either_direction() {
        // 待装的一方声明（实测 dynamic-fps: conflicts {optifabric: "*"}）
        let installed = [item_of(meta_of("optifabric", vec![]), true)];
        let candidate = meta_of(
            "dynamic-fps",
            vec![dep("optifabric", DepKind::Incompatible, "*")],
        );
        let r = check_dependencies(&installed, &candidate, None);
        assert!(r.blocks(), "{r:?}");
        assert_eq!(r.blocked[0].declared_by, "dynamic-fps");

        // **反方向**：冲突由已装的一方声明（实测 sodium: breaks {optifabric: "*"}，
        // 而 optifabric 自己不会声明与 sodium 冲突）。只查待装 mod 的声明，
        // 就会在安装 optifabric 时放行，然后游戏启动时炸——那正是这个检查存在的理由。
        let installed = [item_of(
            meta_of(
                "sodium",
                vec![dep("optifabric", DepKind::Incompatible, "*")],
            ),
            true,
        )];
        let candidate = meta_of("optifabric", vec![]);
        let r = check_dependencies(&installed, &candidate, None);
        assert!(r.blocks(), "反向声明的冲突没被拦住：{r:?}");
        assert_eq!(r.blocked[0].declared_by, "sodium", "声明者是已装的那个");
        assert_eq!(r.blocked[0].id, "optifabric", "冲突对象是待装的那个");
    }

    #[test]
    fn version_constrained_incompatibility_warns_instead_of_blocking() {
        // 实测 21/46 个 jar 有 breaks，且**全都带版本区间**（{"sodium":"<0.6.6"}）。
        // 我们不求值区间，所以只能提醒；拦下来就会误杀"版本其实没问题"的组合。
        let installed = [item_of(meta_of("sodium", vec![]), true)];
        let candidate = meta_of(
            "bettergrassify",
            vec![dep("sodium", DepKind::Incompatible, "<0.6.6")],
        );
        let r = check_dependencies(&installed, &candidate, None);
        assert!(!r.blocks(), "带版本约束的不该被当成无条件冲突：{r:?}");
        assert_eq!(r.uncertain.len(), 1, "{r:?}");
        assert_eq!(r.uncertain[0].constraint, "<0.6.6");
    }

    #[test]
    fn disabled_mods_neither_satisfy_dependencies_nor_conflict() {
        // 禁用的 mod 不会被加载：它不能顶替一个依赖，也不构成冲突
        let installed = [
            item_of(meta_of("cloth-config", vec![]), false),
            item_of(
                meta_of(
                    "sodium",
                    vec![dep("optifabric", DepKind::Incompatible, "*")],
                ),
                false,
            ),
        ];
        let candidate = meta_of(
            "optifabric",
            vec![dep("cloth-config", DepKind::Required, "*")],
        );
        let r = check_dependencies(&installed, &candidate, None);
        assert_eq!(
            r.missing.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["cloth-config"],
            "禁用的 mod 被当成已提供了：{:?}",
            r.missing
        );
        assert!(!r.blocks(), "禁用的 mod 被当成冲突来源了：{:?}", r.blocked);
    }
}
