//! 实例：一份独立的游戏目录 + 它自己的版本与 Java/内存覆盖。
//!
//! 存储就是 `instances/<slug>/instance.json` 一个文件（不引数据库）：实例天然可以被
//! 复制、打包、丢进网盘同步，一个自描述的 JSON 比嵌入式 DB 更适合这个场景。
//! 游戏目录是 `instances/<slug>/.minecraft`，mods/config/saves 都在里面，互不干扰。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::archive::{SKIP_DIRS, skip_file};
use crate::error::Error;
use crate::launch::ServerJoin;
use crate::paths::Paths;

/// 窗口分辨率下限/上限。低于这个尺寸游戏窗口画不出东西，高于它多半是输入错误。
pub const MIN_RESOLUTION: u32 = 160;
pub const MAX_RESOLUTION: u32 = 32768;
/// 内存下限：比这更小 JVM 起不来（连启动画面都到不了）。上限只是挡掉明显的输入错误。
pub const MIN_MEMORY_MB: u32 = 512;
pub const MAX_MEMORY_MB: u32 = 131_072;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[derive(Default)]
pub struct Instance {
    /// 目录名，受 `validate_slug` 约束（挡掉 `..`/路径分隔符）。
    pub slug: String,
    /// 显示名，可以与 slug 不同（允许中文与空格）。
    pub name: String,
    /// 版本 JSON 的 id；装了 loader 后这里是 loader 的继承版本 id（如 `fabric-loader-…-1.21.4`）。
    pub version_id: String,
    pub created_at: u64,
    pub last_launched_at: Option<u64>,
    pub launches: u32,
    /// 最大内存（MiB）；None = 用全局默认。
    pub memory_mb: Option<u32>,
    /// 指定 java 可执行文件；None = 按版本的 majorVersion 自动探测。
    pub java_path: Option<PathBuf>,
    /// 追加的 JVM 参数（在版本自带参数之后、主类之前）。
    pub extra_jvm_args: Vec<String>,
    /// 追加的游戏参数（在所有游戏参数最后，可覆盖版本模板的同名项）。
    pub extra_game_args: Vec<String>,
    /// 启动分辨率；None = 游戏自己决定。
    pub resolution: Option<Resolution>,
    /// 启动后直连的服务器；None = 停在主菜单。
    pub server: Option<ServerJoin>,
    /// 演示模式（不登录正版也能玩，但存档受时间限制）。
    pub demo: bool,
}

/// 用户在设置面板里能改的字段。
///
/// 刻意**不是**整个 `Instance`：启动计数与最近启动时间由启动路径自己维护，
/// 让 UI 回传完整对象会在"改了设置"与"刚启动过"之间互相覆盖。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct InstanceSettings {
    pub name: String,
    pub version_id: String,
    pub memory_mb: Option<u32>,
    pub java_path: Option<PathBuf>,
    pub extra_jvm_args: Vec<String>,
    pub extra_game_args: Vec<String>,
    pub resolution: Option<Resolution>,
    pub server: Option<ServerJoin>,
    pub demo: bool,
}

/// slug 规则：ASCII 字母数字加 `_`/`-`，1–64 字符。不含 `.`（挡掉 `.`/`..` 与隐藏目录），
/// 也不含路径分隔符——否则 `../` 就能写到数据目录外。规则只在这一处。
pub fn validate_slug(slug: &str) -> Result<(), Error> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!("invalid instance slug: {slug:?}")))
    }
}

pub fn instance_dir(paths: &Paths, slug: &str) -> Result<PathBuf, Error> {
    validate_slug(slug)?;
    Ok(paths.instances().join(slug))
}

pub fn game_dir(paths: &Paths, slug: &str) -> Result<PathBuf, Error> {
    Ok(instance_dir(paths, slug)?.join(".minecraft"))
}

pub fn config_file(paths: &Paths, slug: &str) -> Result<PathBuf, Error> {
    Ok(instance_dir(paths, slug)?.join("instance.json"))
}

/// natives 解压目录。每次启动前清空重建（残留上一版本的 .dll/.so 会造成
/// "换了游戏版本之后偶发崩溃"），所以放在实例目录而不是全局缓存里。
pub fn natives_dir(paths: &Paths, slug: &str) -> Result<PathBuf, Error> {
    Ok(instance_dir(paths, slug)?.join("natives"))
}

pub fn load(paths: &Paths, slug: &str) -> Result<Instance, Error> {
    let file = config_file(paths, slug)?;
    let text = std::fs::read_to_string(&file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            // 分成"没有这个实例"与"实例文件坏了"：前者 UI 该刷新列表，后者该报损坏
            Error::NotFound(format!("instance {slug} does not exist"))
        } else {
            Error::Io(e)
        }
    })?;
    let mut inst: Instance = serde_json::from_str(&text)
        .map_err(|e| Error::Config(format!("{}: {e}", file.display())))?;
    // 文件可能被手改或从别处拷来：slug 以目录名为准，避免"目录叫 A、内容说自己是 B"
    inst.slug = slug.to_string();
    Ok(inst)
}

pub fn save(paths: &Paths, inst: &Instance) -> Result<(), Error> {
    let file = config_file(paths, &inst.slug)?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(game_dir(paths, &inst.slug)?)?;
    let json = serde_json::to_vec_pretty(inst).map_err(|e| Error::Config(e.to_string()))?;
    // 原子写：实例配置丢了等于这个实例白配
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &file)?;
    Ok(())
}

/// 列出所有实例（按最近启动时间倒序，没启动过的按创建时间）。
/// 读不出来的目录跳过而不是让整个列表失败：一个坏实例不该让启动器打不开。
pub fn list(paths: &Paths) -> Vec<Instance> {
    let Ok(entries) = std::fs::read_dir(paths.instances()) else {
        return Vec::new();
    };
    let mut out: Vec<Instance> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter_map(|slug| load(paths, &slug).ok())
        .collect();
    out.sort_by(|a, b| {
        let key = |i: &Instance| i.last_launched_at.unwrap_or(i.created_at);
        key(b).cmp(&key(a)).then_with(|| a.name.cmp(&b.name))
    });
    out
}

pub fn remove(paths: &Paths, slug: &str) -> Result<(), Error> {
    let dir = instance_dir(paths, slug)?;
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

/// 校验并规范化设置。空参数丢掉（UI 里的空行是常态，不该变成命令行上的空串），
/// 数值越界直接拒绝：`-Xmx0M` 这种参数会让 JVM 立刻退出，而用户看到的是"启动失败"。
pub fn sanitize_settings(s: InstanceSettings) -> Result<InstanceSettings, Error> {
    let name = s.name.trim().to_string();
    if name.is_empty() {
        return Err(Error::Invalid("instance name cannot be empty".into()));
    }
    let version_id = s.version_id.trim().to_string();
    if version_id.is_empty() {
        return Err(Error::Invalid("instance version cannot be empty".into()));
    }
    if let Some(mb) = s.memory_mb
        && !(MIN_MEMORY_MB..=MAX_MEMORY_MB).contains(&mb)
    {
        return Err(Error::Invalid(format!(
            "memory must be between {MIN_MEMORY_MB} and {MAX_MEMORY_MB} MiB (got {mb})"
        )));
    }
    if let Some(r) = s.resolution {
        let ok = |v: u32| (MIN_RESOLUTION..=MAX_RESOLUTION).contains(&v);
        if !ok(r.width) || !ok(r.height) {
            return Err(Error::Invalid(format!(
                "resolution must be between {MIN_RESOLUTION} and {MAX_RESOLUTION} per side (got {}x{})",
                r.width, r.height
            )));
        }
    }
    if let Some(srv) = &s.server
        && srv.host.trim().is_empty()
    {
        return Err(Error::Invalid("server host cannot be empty".into()));
    }
    // 空路径等于"没有指定"，否则 java 探测会报一句指向空字符串的怪错
    let java_path = s
        .java_path
        .filter(|p| !p.as_os_str().is_empty() && p.to_string_lossy().trim() != "");

    Ok(InstanceSettings {
        name,
        version_id,
        memory_mb: s.memory_mb,
        java_path,
        extra_jvm_args: clean_args(s.extra_jvm_args)?,
        extra_game_args: clean_args(s.extra_game_args)?,
        resolution: s.resolution,
        server: s.server.map(|mut srv| {
            srv.host = srv.host.trim().to_string();
            srv
        }),
        demo: s.demo,
    })
}

/// 用户参数是按行/按空格切出来的，空行与换行都要挡：
/// 换行会污染日志（一行日志变成两行，P5 的崩溃分析按行切分），空串会被 JVM 当成一个参数。
fn clean_args(args: Vec<String>) -> Result<Vec<String>, Error> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if a.contains('\n') || a.contains('\r') {
            return Err(Error::Invalid(format!(
                "launch arguments cannot contain line breaks: {a:?}"
            )));
        }
        let t = a.trim().to_string();
        if !t.is_empty() {
            out.push(t);
        }
    }
    Ok(out)
}

/// 把设置写进实例。**不碰** slug / created_at / launches / last_launched_at：
/// 这些由启动路径维护，让 UI 一起回传会互相覆盖。
pub fn apply_settings(inst: &mut Instance, s: InstanceSettings) {
    inst.name = s.name;
    inst.version_id = s.version_id;
    inst.memory_mb = s.memory_mb;
    inst.java_path = s.java_path;
    inst.extra_jvm_args = s.extra_jvm_args;
    inst.extra_game_args = s.extra_game_args;
    inst.resolution = s.resolution;
    inst.server = s.server;
    inst.demo = s.demo;
}

/// 当前设置（UI 打开设置面板时回填表单用）。
pub fn settings_of(inst: &Instance) -> InstanceSettings {
    InstanceSettings {
        name: inst.name.clone(),
        version_id: inst.version_id.clone(),
        memory_mb: inst.memory_mb,
        java_path: inst.java_path.clone(),
        extra_jvm_args: inst.extra_jvm_args.clone(),
        extra_game_args: inst.extra_game_args.clone(),
        resolution: inst.resolution,
        server: inst.server.clone(),
        demo: inst.demo,
    }
}

// ---------------------------------------------------------------------------
// 复制
// ---------------------------------------------------------------------------

/// 复制出一个新实例（做变体、留档、把整合包分给别人之前先试一遍）。
///
/// 跳过哪些目录由 `archive::SKIP_DIRS` 定义——导出实例必须给出同一个答案，
/// 否则"复制出来的实例"与"导出再导入的实例"不一样。
///
/// 计数与"最近启动"归零：新实例还没被启动过，继承一个假的启动时间会让列表排序骗人。
/// `progress` 每复制完一个文件回调一次（已复制字节, 总字节），UI 据此显示进度条——
/// 一个装满 mod 与存档的实例有几个 GB，没有反馈时用户会以为卡死了。
pub fn duplicate<F>(
    paths: &Paths,
    slug: &str,
    new_slug: &str,
    new_name: &str,
    mut progress: F,
) -> Result<Instance, Error>
where
    F: FnMut(u64, u64),
{
    let src = instance_dir(paths, slug)?;
    let dst = instance_dir(paths, new_slug)?;
    if !src.is_dir() {
        return Err(Error::NotFound(format!("instance {slug} does not exist")));
    }
    if dst.exists() {
        return Err(Error::Invalid(format!(
            "instance already exists: {new_slug}"
        )));
    }
    if new_slug == slug {
        return Err(Error::Invalid("source and destination are the same".into()));
    }

    let total = dir_bytes(&src);
    std::fs::create_dir_all(&dst)?;
    let mut copied = 0u64;
    copy_tree(&src, &dst, &mut |n| {
        copied += n;
        progress(copied, total);
    })?;

    let name = new_name.trim();
    let mut inst = load(paths, slug)?;
    inst.slug = new_slug.to_string();
    inst.name = if name.is_empty() {
        format!("{} (copy)", inst.name)
    } else {
        name.to_string()
    };
    inst.created_at = crate::account::now_unix();
    inst.last_launched_at = None;
    inst.launches = 0;
    save(paths, &inst)?;
    Ok(inst)
}

/// 目录总字节数（按同样的跳过规则），用于进度分母。
fn dir_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else { continue };
            // 符号链接不跟随也不计入：跟过去可能把整棵树复制到别处去
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(e.path());
                }
            } else if !skip_file(&name) {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// 递归拷贝一棵目录树，跳过 `SKIP_DIRS` 与临时文件。
///
/// 实例复制与"导入存档文件夹"共用这一份实现：两套树拷贝迟早会在
/// 符号链接、跳过规则这些细节上分岔，而分岔的那一半通常没人测。
/// 符号链接不跟随（跟过去可能把整棵树复制到别处，或成环）。
pub fn copy_tree<F>(src: &Path, dst: &Path, on_file: &mut F) -> Result<(), Error>
where
    F: FnMut(u64),
{
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().to_string();
        let ft = e.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let to = dst.join(&name);
        if ft.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            std::fs::create_dir_all(&to)?;
            copy_tree(&e.path(), &to, on_file)?;
        } else if !skip_file(&name) {
            std::fs::copy(e.path(), &to)?;
            on_file(std::fs::metadata(&to).map(|m| m.len()).unwrap_or(0));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 导出 / 导入
// ---------------------------------------------------------------------------

/// 导出包里实例配置的固定位置（根目录）。
///
/// 只认这一个位置：压缩包可能把整个实例裹在一层顶层目录里，"找一找再拍平"
/// 是要靠猜的启发式。第三方整合包格式（mrpack / CurseForge）是 P3 的活，
/// 各有各的安装器，不在这里兼容。
const EXPORTED_CONFIG: &str = "instance.json";

/// 导出实例为 zip。返回写进去的文件数与字节数。
///
/// `include_saves = false` 用于"把实例分享出去但不带自己的世界"：
/// 存档通常是实例里最大的部分，而且是**本人的进度**，默认塞进分享包并不合适。
pub fn export(
    paths: &Paths,
    slug: &str,
    dest: &Path,
    include_saves: bool,
) -> Result<crate::archive::ZipSummary, Error> {
    let src = instance_dir(paths, slug)?;
    if !src.is_dir() {
        return Err(Error::NotFound(format!("instance {slug} does not exist")));
    }
    // 目标不能在被打包的目录里：否则边写边读，包会一路长到自己把自己吃进去
    if dest.starts_with(&src) {
        return Err(Error::Invalid(format!(
            "refusing to export into the instance itself: {}",
            dest.display()
        )));
    }
    let extra: &[&str] = if include_saves { &[] } else { &["saves"] };
    crate::archive::zip_dir(&src, dest, extra)
}

/// 导入前的"看一眼"：只读压缩包里的 `instance.json`，**不落盘任何东西**。
///
/// 这一步不是装饰。实例配置里有 `java_path` 与自定义 JVM 参数，
/// 而"一个配置文件能决定启动哪个可执行文件、带什么参数"就是任意命令执行——
/// 正是 P0 删掉 `launch(program, args)` 的理由。导入别人的包等于把那个能力
/// 从数据文件放回来，所以必须让用户在东西落盘之前看见它带了什么。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveInfo {
    pub name: String,
    pub version_id: String,
    pub memory_mb: Option<u32>,
    pub files: usize,
    pub bytes: u64,
    /// 会影响启动行为的内容，UI 必须原样摊开给用户看
    pub extra_jvm_args: Vec<String>,
    pub extra_game_args: Vec<String>,
    pub server: Option<ServerJoin>,
    pub demo: bool,
    /// 包里写了自定义 Java 路径（来自别的机器，导入时会被清空，见 `import`）
    pub had_custom_java: bool,
}

pub fn inspect_archive(archive_path: &Path) -> Result<ArchiveInfo, Error> {
    let Some(text) = crate::archive::read_text_entry(archive_path, EXPORTED_CONFIG)? else {
        return Err(Error::Invalid(format!(
            "{} is not a Compass instance export: no {EXPORTED_CONFIG} at its root",
            archive_path.display()
        )));
    };
    let inst: Instance = serde_json::from_str(&text)
        .map_err(|e| Error::Config(format!("{EXPORTED_CONFIG} is not readable: {e}")))?;
    let (files, bytes) = crate::archive::entries_summary(archive_path)?;
    Ok(ArchiveInfo {
        name: inst.name,
        version_id: inst.version_id,
        memory_mb: inst.memory_mb,
        files,
        bytes,
        extra_jvm_args: inst.extra_jvm_args,
        extra_game_args: inst.extra_game_args,
        server: inst.server,
        demo: inst.demo,
        had_custom_java: inst.java_path.is_some(),
    })
}

/// 导入一个实例导出包。
///
/// **slug 由调用方给，绝不取自压缩包**：包里的 slug 是别人机器上的目录名，
/// 让它决定本地路径等于让一个外部文件决定我们往哪写。
pub fn import(
    paths: &Paths,
    archive_path: &Path,
    slug: &str,
    name: &str,
) -> Result<Instance, Error> {
    let dst = instance_dir(paths, slug)?;
    if dst.exists() {
        return Err(Error::Invalid(format!("instance already exists: {slug}")));
    }
    // 先确认这确实是一个实例包，再解压：反过来会留下一个解压到一半的目录
    let info = inspect_archive(archive_path)?;
    std::fs::create_dir_all(&dst)?;
    // 解压失败时不留半个实例：要么完整可用，要么不存在
    if let Err(e) = crate::archive::unzip_to(archive_path, &dst) {
        let _ = std::fs::remove_dir_all(&dst);
        return Err(e);
    }
    let mut inst = load(paths, slug)?;
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        inst.name = trimmed.to_string();
    } else if inst.name.trim().is_empty() {
        inst.name = info.name;
    }
    // 自定义 Java 路径来自**别的机器**，在这台机器上几乎必然不存在；
    // 留着它只会让第一次启动报一句指向别人硬盘的错误。清空 = 回到按版本要求自动探测。
    inst.java_path = None;
    inst.created_at = crate::account::now_unix();
    inst.last_launched_at = None;
    inst.launches = 0;
    save(paths, &inst)?;
    Ok(inst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("data"));
        p.ensure().unwrap();
        (dir, p)
    }

    fn sample(slug: &str, version: &str) -> Instance {
        Instance {
            slug: slug.into(),
            name: format!("My {slug}"),
            version_id: version.into(),
            created_at: 1_700_000_000,
            ..Instance::default()
        }
    }

    #[test]
    fn slug_rejects_traversal_and_oddities() {
        for bad in ["", ".", "..", "a/b", "a\\b", "a.b", "élan", &"x".repeat(65)] {
            assert!(validate_slug(bad).is_err(), "should reject {bad:?}");
        }
        for good in ["main", "1-21-fabric", "My_Pack-2"] {
            assert!(validate_slug(good).is_ok(), "should accept {good:?}");
        }
        // 目录拼接同样拒绝（不只是校验函数）
        let (_d, p) = paths();
        assert!(instance_dir(&p, "../escape").is_err());
        assert!(game_dir(&p, "..").is_err());
    }

    #[test]
    fn save_creates_game_dir_and_roundtrips() {
        let (_d, p) = paths();
        let inst = sample("main", "1.21.4");
        save(&p, &inst).unwrap();
        assert!(game_dir(&p, "main").unwrap().is_dir());
        let got = load(&p, "main").unwrap();
        assert_eq!(got, inst);
        assert_eq!(got.name, "My main");
    }

    #[test]
    fn load_overrides_slug_from_directory_name() {
        let (_d, p) = paths();
        let mut inst = sample("real", "1.21.4");
        save(&p, &inst).unwrap();
        // 手改文件把 slug 写歪：以目录名为准，否则删除/启动会指向别处
        inst.slug = "pretend".into();
        let file = config_file(&p, "real").unwrap();
        std::fs::write(&file, serde_json::to_vec(&inst).unwrap()).unwrap();
        assert_eq!(load(&p, "real").unwrap().slug, "real");
    }

    #[test]
    fn missing_fields_take_defaults() {
        let (_d, p) = paths();
        let dir = instance_dir(&p, "minimal").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("instance.json"),
            r#"{"slug":"minimal","name":"Minimal","versionId":"1.20.1"}"#,
        )
        .unwrap();
        let got = load(&p, "minimal").unwrap();
        assert_eq!(got.launches, 0);
        assert_eq!(got.memory_mb, None);
        assert_eq!(got.java_path, None);
    }

    #[test]
    fn list_skips_broken_instances_and_sorts_by_recency() {
        let (_d, p) = paths();
        save(&p, &sample("old", "1.20.1")).unwrap();
        let mut recent = sample("recent", "1.21.4");
        recent.last_launched_at = Some(1_800_000_000);
        save(&p, &recent).unwrap();
        // 一个坏实例：不该让整个列表失败
        let broken = instance_dir(&p, "broken").unwrap();
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("instance.json"), "{ nope").unwrap();

        let all = list(&p);
        assert_eq!(all.len(), 2, "{all:?}");
        assert_eq!(all[0].slug, "recent");
        assert_eq!(all[1].slug, "old");
    }

    #[test]
    fn remove_is_idempotent() {
        let (_d, p) = paths();
        save(&p, &sample("gone", "1.21.4")).unwrap();
        remove(&p, "gone").unwrap();
        remove(&p, "gone").unwrap();
        assert!(!instance_dir(&p, "gone").unwrap().exists());
        assert!(list(&p).is_empty());
    }

    fn settings() -> InstanceSettings {
        InstanceSettings {
            name: "生存".into(),
            version_id: "1.21.4".into(),
            memory_mb: Some(6144),
            java_path: None,
            extra_jvm_args: vec!["-Dfoo=bar".into()],
            extra_game_args: vec!["--versionType".into(), "Mythic".into()],
            resolution: Some(Resolution {
                width: 1920,
                height: 1080,
            }),
            server: Some(ServerJoin {
                host: "play.example.com".into(),
                port: Some(25566),
            }),
            demo: false,
        }
    }

    #[test]
    fn settings_roundtrip_through_json() {
        // instance.json 是自描述文件（可能被手改、被拷到别的机器），字段名必须稳定
        let json = serde_json::to_string(&settings()).unwrap();
        assert!(json.contains("\"memoryMb\":6144"), "{json}");
        assert!(json.contains("\"extraJvmArgs\""), "{json}");
        assert!(json.contains("\"width\":1920"), "{json}");
        assert_eq!(
            serde_json::from_str::<InstanceSettings>(&json).unwrap(),
            settings()
        );
    }

    #[test]
    fn sanitize_rejects_values_that_would_break_the_launch() {
        let base = settings();
        let mut bad = base.clone();
        bad.name = "   ".into();
        assert!(sanitize_settings(bad).is_err(), "空名字");

        let mut bad = base.clone();
        bad.version_id = "".into();
        assert!(sanitize_settings(bad).is_err(), "空版本");

        // -Xmx0M 会让 JVM 立刻退出，用户只看到"启动失败"
        for mb in [0u32, 1, 511, 131_073, u32::MAX] {
            let mut bad = base.clone();
            bad.memory_mb = Some(mb);
            assert!(sanitize_settings(bad).is_err(), "内存 {mb} 应当被拒绝");
        }
        assert_eq!(
            sanitize_settings(InstanceSettings {
                memory_mb: Some(MIN_MEMORY_MB),
                ..base.clone()
            })
            .unwrap()
            .memory_mb,
            Some(MIN_MEMORY_MB)
        );

        for (w, h) in [(0u32, 1080u32), (1920, 0), (159, 1080), (1920, 32769)] {
            let mut bad = base.clone();
            bad.resolution = Some(Resolution {
                width: w,
                height: h,
            });
            assert!(sanitize_settings(bad).is_err(), "分辨率 {w}x{h} 应当被拒绝");
        }

        let mut bad = base.clone();
        bad.server = Some(ServerJoin {
            host: "  ".into(),
            port: None,
        });
        assert!(sanitize_settings(bad).is_err(), "空服务器地址");

        // 换行会把一行日志劈成两行（P5 的崩溃分析按行切分）
        for arg in ["-Dfoo=bar\n-Dx=y", "a\r\nb"] {
            let mut bad = base.clone();
            bad.extra_jvm_args = vec![arg.into()];
            assert!(sanitize_settings(bad).is_err(), "{arg:?} 应当被拒绝");
        }
    }

    #[test]
    fn sanitize_drops_blanks_instead_of_rejecting_them() {
        // UI 里的多行文本框几乎总有空行；把它当错误会让"加一个 JVM 参数"变成一件烦人的事
        let s = sanitize_settings(InstanceSettings {
            extra_jvm_args: vec!["".into(), "  ".into(), " -Dfoo=bar ".into()],
            extra_game_args: vec!["".into()],
            java_path: Some(PathBuf::from("   ")),
            server: Some(ServerJoin {
                host: "  play.example.com  ".into(),
                port: None,
            }),
            ..settings()
        })
        .unwrap();
        assert_eq!(s.extra_jvm_args, vec!["-Dfoo=bar"]);
        assert!(s.extra_game_args.is_empty());
        assert_eq!(s.java_path, None, "空路径等于没指定");
        assert_eq!(s.server.unwrap().host, "play.example.com");
    }

    #[test]
    fn apply_settings_keeps_launch_bookkeeping() {
        let (_d, p) = paths();
        let mut inst = sample("main", "1.20.1");
        inst.launches = 7;
        inst.last_launched_at = Some(1_800_000_000);
        inst.created_at = 1_700_000_000;
        save(&p, &inst).unwrap();

        // 启动计数由启动路径维护；设置面板回传的对象里它是旧值，不该被写回去
        let stale = Instance {
            launches: 0,
            last_launched_at: None,
            created_at: 0,
            ..inst.clone()
        };
        apply_settings(&mut inst.clone(), settings_of(&stale));
        let mut target = inst.clone();
        apply_settings(&mut target, settings());
        assert_eq!(target.launches, 7);
        assert_eq!(target.last_launched_at, Some(1_800_000_000));
        assert_eq!(target.created_at, 1_700_000_000);
        assert_eq!(target.slug, "main", "slug 是目录身份，设置面板改不了它");
        // 其余字段确实被更新
        assert_eq!(target.name, "生存");
        assert_eq!(target.version_id, "1.21.4");
        assert_eq!(
            target.resolution,
            Some(Resolution {
                width: 1920,
                height: 1080
            })
        );
        save(&p, &target).unwrap();
        assert_eq!(load(&p, "main").unwrap(), target);
        assert_eq!(settings_of(&load(&p, "main").unwrap()), settings());
    }

    #[test]
    fn duplicate_copies_content_but_not_per_run_noise() {
        let (_d, p) = paths();
        let mut inst = sample("main", "1.21.4");
        inst.launches = 12;
        inst.last_launched_at = Some(1_800_000_000);
        inst.memory_mb = Some(8192);
        save(&p, &inst).unwrap();

        let game = game_dir(&p, "main").unwrap();
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/a.jar"), b"mod").unwrap();
        std::fs::create_dir_all(game.join("saves/world")).unwrap();
        std::fs::write(game.join("saves/world/level.dat"), b"world").unwrap();
        // 每次启动重建的与纯噪音的：复制它们只会让操作变慢
        std::fs::create_dir_all(p.instances().join("main").join("natives")).unwrap();
        std::fs::write(p.instances().join("main").join("natives/lwjgl.dll"), b"x").unwrap();
        std::fs::create_dir_all(game.join("logs")).unwrap();
        std::fs::write(game.join("logs/latest.log"), b"noise").unwrap();
        std::fs::create_dir_all(game.join("crash-reports")).unwrap();
        std::fs::write(game.join("crash-reports/crash-1.txt"), b"noise").unwrap();
        std::fs::write(game.join("hs_err_pid1234.log"), b"noise").unwrap();
        std::fs::write(game.join("options.txt.tmp"), b"noise").unwrap();

        let mut events = 0;
        let copy = duplicate(&p, "main", "main-copy", "副本", |done, total| {
            events += 1;
            assert!(done <= total, "进度不能超过总量：{done}/{total}");
        })
        .unwrap();

        assert_eq!(copy.slug, "main-copy");
        assert_eq!(copy.name, "副本");
        assert_eq!(copy.version_id, "1.21.4");
        assert_eq!(copy.memory_mb, Some(8192), "实例设置要跟着走");
        assert_eq!(copy.launches, 0, "新实例还没被启动过");
        assert_eq!(copy.last_launched_at, None);
        assert!(copy.created_at > inst.created_at);
        assert!(events > 0, "进度回调没被调用，UI 会看起来卡死");

        let new_game = game_dir(&p, "main-copy").unwrap();
        assert_eq!(std::fs::read(new_game.join("mods/a.jar")).unwrap(), b"mod");
        assert_eq!(
            std::fs::read(new_game.join("saves/world/level.dat")).unwrap(),
            b"world"
        );
        assert!(!new_game.join("logs").exists());
        assert!(!new_game.join("crash-reports").exists());
        assert!(!new_game.join("hs_err_pid1234.log").exists());
        assert!(!new_game.join("options.txt.tmp").exists());
        assert!(
            !instance_dir(&p, "main-copy")
                .unwrap()
                .join("natives")
                .exists()
        );
        // 源实例没被改动
        assert_eq!(load(&p, "main").unwrap().launches, 12);
    }

    #[test]
    fn duplicate_refuses_to_clobber_and_needs_a_source() {
        let (_d, p) = paths();
        save(&p, &sample("main", "1.21.4")).unwrap();
        save(&p, &sample("other", "1.21.4")).unwrap();

        let err = duplicate(&p, "main", "other", "x", |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        // 目标实例的内容没被源实例覆盖
        assert_eq!(load(&p, "other").unwrap().name, "My other");

        assert!(
            duplicate(&p, "main", "main", "x", |_, _| {}).is_err(),
            "自己复制自己"
        );
        let err = duplicate(&p, "nope", "fresh", "x", |_, _| {}).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        // slug 规则同样适用于新实例（否则 ../ 能写到数据目录外）
        assert!(duplicate(&p, "main", "../escape", "x", |_, _| {}).is_err());

        // 名字留空时用源名字加后缀，而不是给一个空名字
        let auto = duplicate(&p, "main", "auto", "", |_, _| {}).unwrap();
        assert_eq!(auto.name, "My main (copy)");
    }

    /// 造一个"用过一阵子"的实例：有 mod、有存档，也有启动产生的噪音。
    fn lived_in(p: &Paths, slug: &str) -> Instance {
        let mut inst = sample(slug, "1.21.4");
        inst.memory_mb = Some(8192);
        inst.java_path = Some(PathBuf::from(if cfg!(windows) {
            r"C:\jdk\bin\java.exe"
        } else {
            "/usr/lib/jvm/java-21/bin/java"
        }));
        inst.extra_jvm_args = vec!["-XX:+UseG1GC".into()];
        inst.extra_game_args = vec!["--versionType".into(), "Mythic".into()];
        inst.launches = 9;
        inst.last_launched_at = Some(1_800_000_000);
        save(p, &inst).unwrap();

        let game = game_dir(p, slug).unwrap();
        std::fs::create_dir_all(game.join("mods")).unwrap();
        std::fs::write(game.join("mods/a.jar"), b"mod").unwrap();
        std::fs::create_dir_all(game.join("saves/w/region")).unwrap();
        std::fs::write(game.join("saves/w/level.dat"), b"level").unwrap();
        std::fs::write(game.join("saves/w/region/r.0.0.mca"), b"chunk").unwrap();
        std::fs::create_dir_all(game.join("logs")).unwrap();
        std::fs::write(game.join("logs/latest.log"), b"noise").unwrap();
        std::fs::create_dir_all(instance_dir(p, slug).unwrap().join("natives")).unwrap();
        std::fs::write(
            instance_dir(p, slug).unwrap().join("natives/lwjgl.dll"),
            b"x",
        )
        .unwrap();
        inst
    }

    #[test]
    fn export_then_import_roundtrips() {
        let (d, p) = paths();
        let original = lived_in(&p, "main");
        let zip = d.path().join("export/main.zip");

        let summary = export(&p, "main", &zip, true).unwrap();
        assert!(
            summary.files >= 4,
            "instance.json + mod + level.dat + region：{summary:?}"
        );
        assert!(zip.is_file());

        let imported = import(&p, &zip, "imported", "").unwrap();
        assert_eq!(imported.slug, "imported");
        assert_eq!(imported.name, "My main", "留空名字时沿用包里显示的名字");
        assert_eq!(imported.version_id, original.version_id);
        assert_eq!(imported.memory_mb, Some(8192));
        assert_eq!(imported.extra_jvm_args, vec!["-XX:+UseG1GC".to_string()]);
        assert_eq!(
            imported.extra_game_args,
            vec!["--versionType".to_string(), "Mythic".to_string()]
        );
        // 启动统计归零：这个实例在这台机器上还没被启动过
        assert_eq!(imported.launches, 0);
        assert_eq!(imported.last_launched_at, None);
        // 别人机器上的 Java 路径在这台机器上没有意义，留着只会让第一次启动报错
        assert_eq!(imported.java_path, None, "自定义 Java 路径必须在导入时清空");

        let game = game_dir(&p, "imported").unwrap();
        assert_eq!(std::fs::read(game.join("mods/a.jar")).unwrap(), b"mod");
        assert_eq!(
            std::fs::read(game.join("saves/w/region/r.0.0.mca")).unwrap(),
            b"chunk"
        );
        // 噪音不进包，所以也不该出现在导入结果里
        assert!(!game.join("logs").exists());
        assert!(
            !instance_dir(&p, "imported")
                .unwrap()
                .join("natives")
                .exists()
        );
        // 源实例没被动过
        assert_eq!(load(&p, "main").unwrap().launches, 9);
    }

    #[test]
    fn export_can_leave_the_world_behind() {
        let (d, p) = paths();
        lived_in(&p, "main");
        let zip = d.path().join("share.zip");
        let s = export(&p, "main", &zip, false).unwrap();

        let info = inspect_archive(&zip).unwrap();
        assert_eq!(info.version_id, "1.21.4");
        let imported = import(&p, &zip, "shared", "分享包").unwrap();
        assert_eq!(imported.name, "分享包");
        let game = game_dir(&p, "shared").unwrap();
        assert!(game.join("mods/a.jar").is_file());
        assert!(
            !game.join("saves").exists(),
            "不含存档的分享包不该把别人的世界带过来"
        );
        assert!(s.files > 0);
    }

    #[test]
    fn inspect_reads_the_archive_without_writing_anything() {
        let (d, p) = paths();
        lived_in(&p, "main");
        let zip = d.path().join("a.zip");
        export(&p, "main", &zip, true).unwrap();

        let info = inspect_archive(&zip).unwrap();
        assert_eq!(info.name, "My main");
        assert_eq!(info.version_id, "1.21.4");
        assert_eq!(info.memory_mb, Some(8192));
        assert!(info.had_custom_java, "包里的自定义 Java 路径要被标出来");
        assert_eq!(info.extra_jvm_args, vec!["-XX:+UseG1GC".to_string()]);
        assert!(info.files > 0 && info.bytes > 0);
        // 关键是"看一眼"不产生任何副作用：还没同意导入就不该有目录
        assert_eq!(
            std::fs::read_dir(p.instances())
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            vec!["main".to_string()],
            "inspect_archive 只该读，不该建实例目录"
        );
        // 这个结构要跨 IPC，字段名两侧由 shape_contract 门比对
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"hadCustomJava\":true"), "{json}");
        assert!(json.contains("\"extraJvmArgs\""), "{json}");
    }

    #[test]
    fn import_refuses_archives_that_are_not_instances() {
        let (d, p) = paths();
        // 一个合法的 zip，但根目录没有 instance.json
        let not_instance = d.path().join("mod.zip");
        {
            let f = std::fs::File::create(&not_instance).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let opts: zip::write::SimpleFileOptions = Default::default();
            z.start_file("some-mod.jar", opts).unwrap();
            std::io::Write::write_all(&mut z, b"fake").unwrap();
            z.finish().unwrap();
        }
        let err = import(&p, &not_instance, "x", "").unwrap_err();
        assert!(
            err.to_string().contains("not a Compass instance export"),
            "{err}"
        );
        assert!(
            !instance_dir(&p, "x").unwrap().exists(),
            "失败不该留下半个实例"
        );

        // 根本不是 zip
        let garbage = d.path().join("garbage.zip");
        std::fs::write(&garbage, b"not a zip at all").unwrap();
        assert!(import(&p, &garbage, "y", "").is_err());
        assert!(!instance_dir(&p, "y").unwrap().exists());

        // 目标已存在
        save(&p, &sample("taken", "1.21.4")).unwrap();
        let zip = d.path().join("ok.zip");
        lived_in(&p, "main");
        export(&p, "main", &zip, true).unwrap();
        let err = import(&p, &zip, "taken", "").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(
            load(&p, "taken").unwrap().name,
            "My taken",
            "已有实例不能被覆盖"
        );
    }

    #[test]
    fn import_ignores_the_slug_inside_the_archive() {
        let (d, p) = paths();
        lived_in(&p, "main");
        let zip = d.path().join("a.zip");
        export(&p, "main", &zip, true).unwrap();

        // 包里的 instance.json 写着 slug=main；导入成别的名字后，身份以目录为准
        let imported = import(&p, &zip, "elsewhere", "").unwrap();
        assert_eq!(imported.slug, "elsewhere");
        assert_eq!(load(&p, "elsewhere").unwrap().slug, "elsewhere");
        // 原来的 main 没被覆盖
        assert_eq!(load(&p, "main").unwrap().launches, 9);
        // slug 规则照旧适用于导入
        assert!(import(&p, &zip, "../escape", "").is_err());
    }

    #[test]
    fn export_refuses_to_write_into_the_instance_itself() {
        let (d, p) = paths();
        lived_in(&p, "main");
        let inside = instance_dir(&p, "main").unwrap().join("self.zip");
        let err = export(&p, "main", &inside, true).unwrap_err();
        assert!(
            err.to_string().contains("into the instance itself"),
            "{err}"
        );
        let err = export(&p, "nope", &d.path().join("x.zip"), true).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    }
}
