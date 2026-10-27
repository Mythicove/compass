//! 资源（assets）：索引解析、objects 下载计划、以及 1.8 之前的 virtual 布局。
//!
//! objects 是**内容寻址**的（`objects/<hash 前两位>/<hash>`），因此：
//! - 多个逻辑名可以指向同一个 hash ⇒ 下载计划必须按 hash 去重，否则同一文件下好几遍；
//! - 跨版本天然共享（1.20 与 1.21 的多数音效是同一个 object），全局缓存一份即可。
//!
//! 1.8 之前的游戏不认 objects 布局，还要在 `assets/virtual/<index id>/` 下按逻辑名摆一份。
//! 版本 JSON 与索引里都**没有** `virtual`/`map_to_resources` 标志（实测 1.7.10 的索引只有
//! `objects` 一个键），所以只能按游戏版本判定。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::net::FileTask;

pub const ASSETS_BASE: &str = "https://resources.download.minecraft.net";

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssetIndex {
    /// BTreeMap 而不是 HashMap：下载顺序稳定，进度条与日志才可复现。
    pub objects: BTreeMap<String, AssetObject>,
}

impl AssetIndex {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// 去重后的总字节数（索引里同一 hash 可能出现多次）。
    pub fn total_size(&self) -> u64 {
        let mut seen = HashSet::new();
        self.objects
            .values()
            .filter(|o| seen.insert(o.hash.clone()))
            .map(|o| o.size)
            .sum()
    }
}

/// 资源对象在**本地缓存**里的相对路径：`objects/<aa>/<hash>`。
/// hash 不是 40 位十六进制时返回 None（宁可少下一个，也不要写出怪路径）。
pub fn object_rel_path(hash: &str) -> Option<String> {
    valid_hash(hash)?;
    Some(format!("objects/{}/{}", &hash[..2], hash))
}

/// 资源对象的**下载 URL**：`<base>/<aa>/<hash>`。
///
/// 注意它比本地路径**少一层 `objects/`**——这是实测出来的（三平台 CI 与本地一致地
/// 对带 `objects/` 的 URL 返回 404，去掉后 200 且 sha1 与索引相符）。
/// 本地布局是我们自己的选择，URL 布局是 Mojang 的，两者不该互相推导。
pub fn object_url(hash: &str) -> Option<String> {
    valid_hash(hash)?;
    Some(format!("{ASSETS_BASE}/{}/{}", &hash[..2], hash))
}

/// 资源索引在缓存里的落点：`indexes/<id>.json`。
pub fn index_rel_path(id: &str) -> String {
    format!("indexes/{id}.json")
}

fn valid_hash(hash: &str) -> Option<()> {
    if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(())
    } else {
        None
    }
}

/// 下载计划：**按 hash 去重**后的一组文件任务。
pub fn download_tasks(index: &AssetIndex, assets_root: &Path) -> Vec<FileTask> {
    let mut seen = HashSet::new();
    let mut tasks = Vec::new();
    for object in index.objects.values() {
        if !seen.insert(object.hash.clone()) {
            continue;
        }
        let (Some(rel), Some(url)) = (object_rel_path(&object.hash), object_url(&object.hash))
        else {
            continue;
        };
        tasks.push(FileTask {
            url,
            dest: assets_root.join(rel),
            sha1: object.hash.clone(),
            size: object.size,
        });
    }
    tasks
}

/// 1.8 之前的 virtual 布局：`(object 文件, assets/virtual/<index_id>/<逻辑名>)`。
/// 用硬链接优先（省磁盘），失败再复制——跨卷或文件系统不支持时硬链接会失败。
pub fn virtual_copies(
    index: &AssetIndex,
    assets_root: &Path,
    index_id: &str,
) -> Vec<(PathBuf, PathBuf)> {
    let root = assets_root.join("virtual").join(index_id);
    index
        .objects
        .iter()
        .filter_map(|(name, object)| {
            let rel = object_rel_path(&object.hash)?;
            Some((assets_root.join(rel), root.join(name)))
        })
        .collect()
}

/// 只有 1.8 之前的游戏需要 virtual 布局。
/// 解析不出版本号（快照如 "26.4-snapshot-6"、远古 "rd-132211"）时按"不需要"处理：
/// 快照都是现代的，远古版本不在支持范围内（计划决策 5）。
pub fn needs_virtual_layout(game_version: &str) -> bool {
    crate::version::version_parts(game_version)
        .is_some_and(|(major, minor, _)| (major, minor) < (1, 8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trimmed() -> AssetIndex {
        AssetIndex::parse(include_str!("../tests/fixtures/assets-19-trimmed.json")).unwrap()
    }

    #[test]
    fn parses_real_asset_index() {
        let index = trimmed();
        assert_eq!(index.objects.len(), 13);
        let first = index.objects.values().next().unwrap();
        assert_eq!(first.hash.len(), 40);
        assert!(first.size > 0);
    }

    #[test]
    fn object_paths_are_content_addressed() {
        let hash = "b62ca8ec10d07e6bf5ac8dae0c8c1d2e6a1e3356";
        // 本地布局有 objects/ 这一层……
        assert_eq!(
            object_rel_path(hash).unwrap(),
            "objects/b6/b62ca8ec10d07e6bf5ac8dae0c8c1d2e6a1e3356"
        );
        // ……而下载 URL 没有。两者不能互相推导（实测带 objects/ 的 URL 是 404）。
        assert_eq!(
            object_url(hash).unwrap(),
            "https://resources.download.minecraft.net/b6/b62ca8ec10d07e6bf5ac8dae0c8c1d2e6a1e3356"
        );
        // 畸形 hash 不该拼出路径
        assert!(object_rel_path("nope").is_none());
        assert!(object_url("nope").is_none());
        assert!(object_rel_path("").is_none());
        assert!(object_rel_path(&"g".repeat(40)).is_none());
        assert_eq!(index_rel_path("19"), "indexes/19.json");
    }

    #[test]
    fn download_tasks_dedupe_by_hash() {
        let index = trimmed(); // 夹具里刻意放了两条指向同一 hash 的逻辑名
        let root = Path::new("/data/cache/assets");
        let tasks = download_tasks(&index, root);
        assert_eq!(tasks.len(), index.objects.len() - 1, "重复 hash 只下一次");
        let mut hashes: Vec<_> = tasks.iter().map(|t| t.sha1.clone()).collect();
        let before = hashes.len();
        hashes.sort();
        hashes.dedup();
        assert_eq!(hashes.len(), before);
        for t in &tasks {
            assert!(t.dest.starts_with(root.join("objects")));
            assert_eq!(t.sha1.len(), 40);
            assert!(t.url.starts_with(ASSETS_BASE));
            // 校验和就是文件名本身：内容寻址的好处
            assert!(t.dest.to_string_lossy().contains(&t.sha1));
        }
        assert_eq!(
            index.total_size(),
            tasks.iter().map(|t| t.size).sum::<u64>()
        );
    }

    #[test]
    fn virtual_layout_only_for_pre_1_8() {
        assert!(needs_virtual_layout("1.7.10"));
        assert!(needs_virtual_layout("1.7"));
        assert!(needs_virtual_layout("1.6.4"));
        assert!(!needs_virtual_layout("1.8"));
        assert!(!needs_virtual_layout("1.12.2"));
        assert!(!needs_virtual_layout("1.21.4"));
        assert!(!needs_virtual_layout("26.4-snapshot-6"));
        assert!(!needs_virtual_layout("rd-132211"));
        assert!(!needs_virtual_layout(""));
    }

    #[test]
    fn virtual_copies_map_hash_to_logical_name() {
        let index = trimmed();
        let root = Path::new("/data/cache/assets");
        let copies = virtual_copies(&index, root, "1.7.10");
        assert_eq!(copies.len(), index.objects.len());
        let (src, dst) = &copies[0];
        assert!(src.starts_with(root.join("objects")));
        assert!(dst.starts_with(root.join("virtual/1.7.10")));
        // 目标用逻辑名（含子目录），源用内容寻址路径
        assert!(dst.to_string_lossy().contains("icons/"));
    }
}
