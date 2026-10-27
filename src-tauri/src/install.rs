//! 安装一个版本：版本 JSON（含继承）→ client jar + 库 + 资源 + 日志配置，全部落**全局缓存**。
//!
//! 装完的产物是可复用的：同一版本的多个实例不会重复下载（assets 按内容寻址、libraries 按
//! Maven 坐标、versions 按 id 存放，都在 cache 下）。实例目录只放游戏数据。
//!
//! 计划任务（`plan`）与执行（`install`）是分开的：前者是纯函数，可以对着真实版本 JSON
//! 做契约测试而不碰网络；后者才下载。

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::assets::{self, AssetIndex};
use crate::error::Error;
use crate::meta::MetaClient;
use crate::net::{Downloader, FileTask, ProgressSink, Summary, format_failures};
use crate::paths::Paths;
use crate::version::{self, Features, LibraryKind, OsInfo, VersionJson};

/// 安装结果：启动所需的一切（合并后的版本 JSON 是启动参数的唯一来源）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledVersion {
    /// 请求安装的 id（装了 loader 后是继承版本的 id）。
    pub id: String,
    /// 实际提供 client jar 的版本 id（继承型版本指向父版本）。
    pub jar_version: String,
    pub asset_index_id: String,
    pub main_class: String,
    pub java_major: u32,
    pub download: Summary,
}

/// 下载计划。
#[derive(Debug, Clone)]
pub struct Plan {
    pub tasks: Vec<FileTask>,
    /// 合并后的版本 JSON 落盘位置：`versions/<id>/<id>.json`。
    pub version_file: PathBuf,
}

pub struct Installer {
    meta: MetaClient,
    dl: Downloader,
    paths: Paths,
}

impl Installer {
    pub fn new(meta: MetaClient, dl: Downloader, paths: Paths) -> Self {
        Self { meta, dl, paths }
    }

    /// 生成下载计划（纯函数，不碰网络）。
    ///
    /// 库解析必须与启动时用**同一套** OsInfo/Features，否则会出现"装了但 classpath 里没有"
    /// 或"classpath 里有一个没下载的文件"——两者都表现为极难诊断的启动失败。
    pub fn plan(&self, id: &str, merged: &VersionJson, os: &OsInfo) -> Result<Plan, Error> {
        let mut tasks = Vec::new();

        // client jar 来自实际提供 jar 的那个版本（继承型版本自己没有 downloads）
        let client = merged
            .downloads
            .as_ref()
            .and_then(|d| d.client.as_ref())
            .ok_or_else(|| Error::Invalid(format!("version {id} has no client jar download")))?;
        let jar_version = merged.jar_version().to_string();
        tasks.push(FileTask {
            url: client.url.clone(),
            dest: self.client_jar_path(&jar_version),
            sha1: client.sha1.clone(),
            size: client.size,
        });

        // 库（含老版本要解压的 natives jar：下载在安装期，解压在启动期）
        for lib in version::resolve_libraries(merged, os, &Features::new()) {
            tasks.push(FileTask {
                url: lib.url,
                dest: self.paths.libraries().join(&lib.path),
                sha1: lib.sha1,
                size: lib.size,
            });
        }

        // 日志配置（Log4Shell 之后必须显式传 -Dlog4j.configurationFile）
        if let Some(logging) = merged.logging.as_ref().and_then(|l| l.client.as_ref()) {
            tasks.push(FileTask {
                url: logging.file.url.clone(),
                dest: self.version_dir(id).join(&logging.file.id),
                sha1: logging.file.sha1.clone(),
                size: logging.file.size,
            });
        }

        // 资源索引本身也是一个要校验的文件
        if let Some(index) = &merged.asset_index {
            tasks.push(FileTask {
                url: index.url.clone(),
                dest: self.paths.assets().join(assets::index_rel_path(&index.id)),
                sha1: index.sha1.clone(),
                size: index.size,
            });
        }

        Ok(Plan {
            tasks,
            version_file: self.version_dir(id).join(format!("{id}.json")),
        })
    }

    /// 安装（幂等）：已就位的文件跳过，重复调用只补差。
    pub async fn install(
        &self,
        id: &str,
        progress: Option<ProgressSink>,
    ) -> Result<InstalledVersion, Error> {
        let os = OsInfo::current();
        let manifest = self.meta.manifest().await?;
        let entry = manifest
            .by_id(id)
            .ok_or_else(|| Error::Invalid(format!("unknown version: {id}")))?
            .clone();
        let merged = self.meta.resolve_version(&entry).await?;

        let plan = self.plan(id, &merged, &os)?;
        std::fs::create_dir_all(self.version_dir(id))?;
        // 合并后的版本 JSON 落盘：启动时不再联网取元数据（离线也能启动已装好的版本）
        std::fs::write(
            &plan.version_file,
            serde_json::to_vec_pretty(&merged).map_err(|e| Error::Config(e.to_string()))?,
        )?;

        let mut summary = self.dl.download_all(plan.tasks, progress.clone()).await;
        if !summary.failed.is_empty() {
            return Err(Error::Config(format!(
                "failed to download {} file(s): {}",
                summary.failed.len(),
                format_failures(&summary.failed)
            )));
        }

        // 资源对象：必须先有索引文件才能知道要下什么，故作为第二批
        let index_id = merged.asset_index_id().unwrap_or_default().to_string();
        let index = self.load_asset_index(&merged, &index_id).await?;
        let asset_tasks = assets::download_tasks(&index, &self.paths.assets());
        let assets_summary = self.dl.download_all(asset_tasks, progress).await;
        if !assets_summary.failed.is_empty() {
            return Err(Error::Config(format!(
                "failed to download {} asset(s): {}",
                assets_summary.failed.len(),
                format_failures(&assets_summary.failed)
            )));
        }
        summary.downloaded += assets_summary.downloaded;
        summary.skipped += assets_summary.skipped;
        summary.bytes += assets_summary.bytes;

        // 1.8 之前：还要在 assets/virtual/<index id>/ 摆一份逻辑名布局
        if assets::needs_virtual_layout(merged.jar_version()) || assets::needs_virtual_layout(id) {
            materialize_virtual(&index, &self.paths.assets(), &index_id)?;
        }

        Ok(InstalledVersion {
            id: id.to_string(),
            jar_version: merged.jar_version().to_string(),
            asset_index_id: index_id,
            main_class: merged.main_class.clone().unwrap_or_default(),
            java_major: merged
                .java_version
                .as_ref()
                .map(|j| j.major_version)
                .unwrap_or(8),
            download: summary,
        })
    }

    /// 读已下载的资源索引（安装流程里它刚被下载并校验过）。
    async fn load_asset_index(
        &self,
        merged: &VersionJson,
        index_id: &str,
    ) -> Result<AssetIndex, Error> {
        let path = self.paths.assets().join(assets::index_rel_path(index_id));
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| Error::Config(format!("asset index {} missing ({}): {e}", index_id, e)))?;
        AssetIndex::parse(&text).map_err(|e| {
            Error::Config(format!(
                "asset index {} (from {}) is malformed: {e}",
                index_id, merged.id
            ))
        })
    }

    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.paths.versions().join(id)
    }

    pub fn client_jar_path(&self, jar_version: &str) -> PathBuf {
        self.version_dir(jar_version)
            .join(format!("{jar_version}.jar"))
    }

    pub fn version_json_path(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.json"))
    }
}

/// 把 objects 里的内容按逻辑名摆到 `assets/virtual/<index_id>/`。
/// 硬链接优先（同一份内容不占两份磁盘），失败再复制——跨卷或文件系统不支持时会失败。
pub fn materialize_virtual(
    index: &AssetIndex,
    assets_root: &Path,
    index_id: &str,
) -> Result<(), Error> {
    for (src, dst) in assets::virtual_copies(index, assets_root, index_id) {
        if dst.exists() {
            continue;
        }
        if !src.exists() {
            return Err(Error::Config(format!(
                "asset object missing before virtual layout: {}",
                src.display()
            )));
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::hard_link(&src, &dst).is_err() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// natives jar 里哪些条目该跳过（版本 JSON 的 extract.exclude，通常就是 META-INF/）。
pub fn is_excluded(entry: &str, exclude: &[String]) -> bool {
    exclude.iter().any(|prefix| entry.starts_with(prefix))
}

/// 把已解析的库列表分成 classpath 与待解压的 natives 两组。
pub fn split_libraries(
    merged: &VersionJson,
    os: &OsInfo,
    paths: &Paths,
) -> (Vec<PathBuf>, Vec<(PathBuf, Vec<String>)>) {
    let mut classpath = Vec::new();
    let mut natives = Vec::new();
    for lib in version::resolve_libraries(merged, os, &Features::new()) {
        let path = paths.libraries().join(&lib.path);
        match lib.kind {
            LibraryKind::Classpath => classpath.push(path),
            LibraryKind::Native { exclude } => natives.push((path, exclude)),
        }
    }
    (classpath, natives)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::VersionJson;

    fn installer() -> (tempfile::TempDir, Installer) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("data"));
        paths.ensure().unwrap();
        let http = reqwest::Client::new();
        let inst = Installer::new(
            MetaClient::new(http.clone(), ""),
            Downloader::new(http, 8, ""),
            paths,
        );
        (dir, inst)
    }

    fn parse(name: &str, json: &str) -> VersionJson {
        VersionJson::parse(json).unwrap_or_else(|e| panic!("parse {name}: {e}"))
    }

    #[test]
    fn plan_covers_jar_libraries_logging_and_index() {
        let (_d, inst) = installer();
        let v = parse("1.21.4", include_str!("../tests/fixtures/1.21.4.json"));
        let os = OsInfo::new(
            crate::version::OsName::Windows,
            "10.0",
            crate::version::Arch::X64,
        );
        let plan = inst.plan("1.21.4", &v, &os).unwrap();

        let jar = plan
            .tasks
            .iter()
            .find(|t| t.dest.ends_with("1.21.4.jar"))
            .expect("client jar");
        assert!(jar.url.ends_with("/client.jar"));
        assert_eq!(jar.sha1.len(), 40);
        assert_eq!(jar.size, 28_335_587);
        assert!(
            jar.dest.starts_with(inst.paths.versions()),
            "{:?}",
            jar.dest
        );

        // 库：条数与 resolve_libraries 一致，且都落在 cache/libraries 下
        let libs = version::resolve_libraries(&v, &os, &Features::new());
        let lib_tasks: Vec<_> = plan
            .tasks
            .iter()
            .filter(|t| t.dest.starts_with(inst.paths.libraries()))
            .collect();
        assert_eq!(lib_tasks.len(), libs.len());
        for t in lib_tasks {
            assert!(t.url.starts_with("http"), "{:?}", t.url);
        }

        // 日志配置：文件名来自 logging.client.file.id
        assert!(
            plan.tasks
                .iter()
                .any(|t| t.dest.ends_with("client-1.21.2.xml"))
        );
        // 资源索引本身也要下载并校验
        let index = plan
            .tasks
            .iter()
            .find(|t| t.dest.ends_with("indexes/19.json"))
            .expect("asset index");
        assert_eq!(index.sha1.len(), 40);
        // 版本 JSON 落盘位置
        assert!(plan.version_file.ends_with("versions/1.21.4/1.21.4.json"));
    }

    #[test]
    fn plan_for_inherited_version_uses_parent_jar() {
        let (_d, inst) = installer();
        let parent = parse("1.21.4", include_str!("../tests/fixtures/1.21.4.json"));
        let child = parse(
            "fabric",
            include_str!("../tests/fixtures/fabric-1.21.4.json"),
        );
        let merged = version::merge(parent, child);
        let os = OsInfo::new(
            crate::version::OsName::Windows,
            "10.0",
            crate::version::Arch::X64,
        );
        let id = "fabric-loader-0.20.1-1.21.4";

        let plan = inst.plan(id, &merged, &os).unwrap();
        // client jar 用父版本的名字与目录，版本 JSON 用子版本的名字
        assert!(
            plan.tasks
                .iter()
                .any(|t| t.dest.ends_with("versions/1.21.4/1.21.4.jar"))
        );
        assert!(
            plan.version_file
                .ends_with(format!("versions/{id}/{id}.json"))
        );
        // Fabric 的库（无 downloads 块）也要进计划，URL 来自它自己的 Maven 基址
        assert!(
            plan.tasks
                .iter()
                .any(|t| t.url.starts_with("https://maven.fabricmc.net/"))
        );
        // 没有校验和的那两条（intermediary/loader）用空 sha1 表示"无从校验"
        let loader = plan
            .tasks
            .iter()
            .find(|t| t.dest.to_string_lossy().contains("fabric-loader"))
            .expect("fabric-loader jar");
        assert!(loader.sha1.is_empty());
    }

    #[test]
    fn plan_includes_legacy_natives_for_extraction_at_launch() {
        let (_d, inst) = installer();
        let v = parse("1.12.2", include_str!("../tests/fixtures/1.12.2.json"));
        let os = OsInfo::new(
            crate::version::OsName::Windows,
            "10.0",
            crate::version::Arch::X64,
        );
        let plan = inst.plan("1.12.2", &v, &os).unwrap();
        assert!(plan.tasks.iter().any(|t| {
            t.dest
                .to_string_lossy()
                .contains("lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar")
        }));
        // 1.12.2 没有 logging.client（那套是 1.19+ 才有的），计划里就不该有日志文件
        assert!(
            !plan
                .tasks
                .iter()
                .any(|t| t.dest.to_string_lossy().contains("client-1.2"))
        );
    }

    #[test]
    fn plan_without_client_download_is_an_error() {
        let (_d, inst) = installer();
        // Fabric 的子版本自己没有 downloads.client：单独安装它是没有意义的，必须与父版本合并
        let child = parse(
            "fabric",
            include_str!("../tests/fixtures/fabric-1.21.4.json"),
        );
        let os = OsInfo::new(
            crate::version::OsName::Windows,
            "10.0",
            crate::version::Arch::X64,
        );
        let err = inst
            .plan("fabric-loader-0.20.1-1.21.4", &child, &os)
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }

    #[test]
    fn split_libraries_separates_classpath_from_natives() {
        let (_d, inst) = installer();
        // 1.12.2/Windows：老式 natives 要解压，普通库进 classpath
        let v = parse("1.12.2", include_str!("../tests/fixtures/1.12.2.json"));
        let os = OsInfo::new(
            crate::version::OsName::Windows,
            "10.0",
            crate::version::Arch::X64,
        );
        let (cp, natives) = split_libraries(&v, &os, &inst.paths);
        assert!(!cp.is_empty());
        assert!(natives.iter().any(|(p, exclude)| {
            p.to_string_lossy()
                .contains("lwjgl-platform-2.9.4-nightly-20150209-natives-windows.jar")
                && exclude == &vec!["META-INF/".to_string()]
        }));
        assert!(cp.iter().all(|p| p.starts_with(inst.paths.libraries())));

        // 1.21.4：现代 LWJGL3 全在 classpath，没有要解压的。
        // 三个平台的条数各不相同（113 条里被 rules 刷掉的数量不同），钉死具体数字：
        // 规则求值一旦回归，这里会立刻炸，而不是等到游戏里少个 natives 才神秘崩溃。
        let modern = parse("1.21.4", include_str!("../tests/fixtures/1.21.4.json"));
        let (cp2, natives2) = split_libraries(&modern, &os, &inst.paths);
        assert!(natives2.is_empty());
        assert_eq!(cp2.len(), 84, "windows should keep 84 of 113 libraries");
        let mac = OsInfo::new(
            crate::version::OsName::Osx,
            "14.5",
            crate::version::Arch::Arm64,
        );
        assert_eq!(split_libraries(&modern, &mac, &inst.paths).0.len(), 77);
        let nix = OsInfo::new(
            crate::version::OsName::Linux,
            "6.8.0",
            crate::version::Arch::X64,
        );
        assert_eq!(split_libraries(&modern, &nix, &inst.paths).0.len(), 70);
    }

    #[test]
    fn extract_exclude_is_prefix_based() {
        assert!(is_excluded(
            "META-INF/MANIFEST.MF",
            &["META-INF/".to_string()]
        ));
        assert!(!is_excluded("org/lwjgl/x.dll", &["META-INF/".to_string()]));
        assert!(!is_excluded("anything", &[]));
    }

    #[test]
    fn virtual_layout_links_objects_into_logical_names() {
        let dir = tempfile::tempdir().unwrap();
        let assets_root = dir.path().join("assets");
        let index =
            AssetIndex::parse(include_str!("../tests/fixtures/assets-19-trimmed.json")).unwrap();
        // 先造出 objects（真实流程里它们是下载来的）
        for task in assets::download_tasks(&index, &assets_root) {
            std::fs::create_dir_all(task.dest.parent().unwrap()).unwrap();
            std::fs::write(&task.dest, b"payload").unwrap();
        }
        materialize_virtual(&index, &assets_root, "1.7.10").unwrap();
        let first = index.objects.keys().next().unwrap();
        let linked = assets_root.join("virtual/1.7.10").join(first);
        assert!(linked.exists(), "missing {}", linked.display());
        assert_eq!(std::fs::read(&linked).unwrap(), b"payload");
        // 幂等
        materialize_virtual(&index, &assets_root, "1.7.10").unwrap();
    }

    #[test]
    fn virtual_layout_fails_loudly_when_object_missing() {
        let dir = tempfile::tempdir().unwrap();
        let assets_root = dir.path().join("assets");
        let index =
            AssetIndex::parse(include_str!("../tests/fixtures/assets-19-trimmed.json")).unwrap();
        let err = materialize_virtual(&index, &assets_root, "1.7.10").unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{err:?}");
    }
}
