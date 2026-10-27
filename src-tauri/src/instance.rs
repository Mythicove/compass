//! 实例：一份独立的游戏目录 + 它自己的版本与 Java/内存覆盖。
//!
//! 存储就是 `instances/<slug>/instance.json` 一个文件（不引数据库）：实例天然可以被
//! 复制、打包、丢进网盘同步，一个自描述的 JSON 比嵌入式 DB 更适合这个场景。
//! 游戏目录是 `instances/<slug>/.minecraft`，mods/config/saves 都在里面，互不干扰。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::paths::Paths;

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
    let text = std::fs::read_to_string(&file)?;
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
}
