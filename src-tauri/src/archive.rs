//! zip 读写：实例导出/导入用它，natives 解压也用它。
//!
//! 放在一个模块里的理由是**zip-slip 防护只该有一份实现**。这个 crate 里已经有
//! 一处（`launch::extract_natives`），P3 的整合包安装还会再来一处；三处各写一遍
//! 的话，迟早有一处忘了检查 `..`，而那一处就是"下载的压缩包能写到任意路径"。
//!
//! 导出/导入的跳过规则也定义在这里（`SKIP_DIRS` / `skip_file`）：实例复制与实例导出
//! 对"什么不算实例内容"必须给出同一个答案，否则复制出来的实例和导出再导入的实例不一样。

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Component, Path, PathBuf};

use crate::error::Error;

/// 复制/导出实例时跳过的目录：都是每次启动重建、或纯噪音、或含隐私的东西。
/// - `natives` 每次启动前清空重建（见 `instance::natives_dir`）
/// - `logs` / `crash-reports` 可能含账号名与机器路径，且能长到几百 MB
/// - `quickPlay` 是 Quick Play 的日志目录
pub const SKIP_DIRS: [&str; 4] = ["natives", "logs", "crash-reports", "quickPlay"];

/// 跳过的文件：JVM 崩溃转储、编辑器临时文件、macOS 元数据。
pub fn skip_file(name: &str) -> bool {
    name.starts_with("hs_err_pid") || name.ends_with(".tmp") || name == ".DS_Store"
}

/// 导出结果（UI 报"导出了多少个文件、多大"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipSummary {
    pub files: usize,
    pub bytes: u64,
}

/// zip 条目名 → 目标相对路径。
///
/// 拒绝绝对路径与任何 `..` 分量。条目名来自压缩包，也就是来自**别人给的文件**：
/// 一个 `../../../../.ssh/authorized_keys` 就是一次任意写入。
pub fn safe_rel_path(name: &str) -> Result<PathBuf, Error> {
    let rel = Path::new(name);
    if rel.is_absolute() || rel.has_root() {
        return Err(Error::Invalid(format!(
            "refusing absolute entry {name:?} in archive"
        )));
    }
    if rel.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(Error::Invalid(format!(
            "refusing unsafe entry {name:?} in archive"
        )));
    }
    // Windows 上 `C:foo` 这类"驱动器相对路径"也要挡（has_root 覆盖不到）
    let mut out = PathBuf::new();
    for c in rel.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => {
                return Err(Error::Invalid(format!(
                    "refusing unsafe entry {name:?} in archive"
                )));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(Error::Invalid(format!("empty entry name {name:?}")));
    }
    Ok(out)
}

/// 把一个目录打包成 zip。`extra_skip_dirs` 用于"导出时不含存档"这类按需排除。
///
/// 条目名一律用 `/` 分隔的相对路径（zip 的约定），符号链接不跟随。
pub fn zip_dir(
    src_root: &Path,
    dest: &Path,
    extra_skip_dirs: &[&str],
) -> Result<ZipSummary, Error> {
    if !src_root.is_dir() {
        return Err(Error::NotFound(format!(
            "{} is not a directory",
            src_root.display()
        )));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(dest)?;
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut files = 0usize;
    let mut bytes = 0u64;
    // 显式栈而不是递归：存档目录可以很深，而递归深度不该由别人的数据决定
    let mut stack: Vec<(PathBuf, String)> = vec![(src_root.to_path_buf(), String::new())];
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
                if SKIP_DIRS.contains(&name.as_str()) || extra_skip_dirs.contains(&name.as_str()) {
                    continue;
                }
                stack.push((path, format!("{rel}/")));
            } else if !skip_file(&name) {
                let data = std::fs::read(&path)?;
                bytes += data.len() as u64;
                files += 1;
                zip.start_file(&rel, opts)?;
                std::io::Write::write_all(&mut zip, &data)?;
            }
        }
    }
    zip.finish()?;
    Ok(ZipSummary { files, bytes })
}

/// 解压到目标目录，返回写入的文件数。目标目录必须已存在且为空（调用方负责）。
pub fn unzip_to(archive: &Path, dest_root: &Path) -> Result<usize, Error> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| Error::Archive(zip::result::ZipError::Io(std::io::Error::other(e))))?;
    std::fs::create_dir_all(dest_root)?;
    let mut count = 0usize;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().to_string();
        if entry.is_dir() {
            continue;
        }
        let rel = safe_rel_path(&name)?;
        let out = dest_root.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::with_capacity(entry.size().min(8 * 1024 * 1024) as usize);
        entry.read_to_end(&mut buf)?;
        std::fs::write(&out, buf)?;
        count += 1;
    }
    Ok(count)
}

/// 压缩包的条目数与解压后总字节数（导入前给用户看"这有多大"）。
pub fn entries_summary(archive: &Path) -> Result<(usize, u64), Error> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        files += 1;
        bytes += entry.size();
    }
    Ok((files, bytes))
}

/// 只读压缩包里的一个文本条目，**不解压整个包**。
///
/// 导入前先看一眼里面是什么（`instance.json`）靠它：让用户在东西落盘之前
/// 就知道这个实例带了哪些自定义参数，而不是导入之后才发现。
pub fn read_text_entry(archive: &Path, entry_name: &str) -> Result<Option<String>, Error> {
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))?;
    let Some(mut entry) = zip.by_name(entry_name).ok() else {
        return Ok(None);
    };
    // 一个 instance.json 是几百字节；读到 1MiB 以上说明这不是我们要的东西
    if entry.size() > 1024 * 1024 {
        return Err(Error::Invalid(format!(
            "{entry_name} in archive is {} bytes — too large to be a config file",
            entry.size()
        )));
    }
    let mut text = String::new();
    entry.read_to_string(&mut text)?;
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn safe_rel_path_blocks_every_escape_form() {
        // 这些在**任何**平台都必须被拒绝
        for bad in [
            "../escape.txt",
            "a/../../escape.txt",
            "/etc/passwd",
            "./../x",
            "",
            ".",
        ] {
            assert!(safe_rel_path(bad).is_err(), "should reject {bad:?}");
        }
        for good in ["instance.json", ".minecraft/mods/a.jar", "./a/b.jar"] {
            assert!(safe_rel_path(good).is_ok(), "should accept {good:?}");
        }
        assert_eq!(
            safe_rel_path(".minecraft/mods/a.jar").unwrap(),
            PathBuf::from(".minecraft").join("mods").join("a.jar")
        );

        // 反斜杠与盘符**只在 Windows 上**是分隔符/前缀。在 Unix 上 `..\escape.txt`
        // 只是一个名字里带反斜杠的普通文件：解压出来是 dest 里一个叫这个名字的文件，
        // 不构成穿越。所以按平台断言各自真正被保证的东西——
        // 把 Windows 专属的形式当成"所有平台都该拒绝"，CI 上就是 macOS/Linux 一起红。
        let windows_only = [
            "..\\escape.txt",
            "C:\\Windows\\system32\\x.dll",
            "C:evil.dll",
        ];
        if cfg!(windows) {
            for bad in windows_only {
                assert!(safe_rel_path(bad).is_err(), "windows should reject {bad:?}");
            }
        } else {
            for name in windows_only {
                let rel = safe_rel_path(name)
                    .unwrap_or_else(|e| panic!("unix 上 {name:?} 不是穿越，不该被拒：{e}"));
                // 关键不变量：拼到目标目录后面仍然在目标目录里面
                assert!(!rel.is_absolute(), "{name:?} → {rel:?}");
                assert!(
                    !rel.components()
                        .any(|c| matches!(c, Component::ParentDir | Component::RootDir)),
                    "{name:?} → {rel:?}"
                );
            }
        }
    }

    #[test]
    fn joined_destination_never_escapes_for_any_rejected_or_accepted_entry() {
        // 上一条测的是"哪些名字被拒"；这条测的是**真正要保证的事**：
        // 凡是 safe_rel_path 放行的名字，拼到任意目标目录后都仍在该目录内。
        let dest = Path::new("/tmp/instance/.minecraft");
        for name in [
            "instance.json",
            ".minecraft/mods/a.jar",
            "a/b/c/d.txt",
            "..\\escape.txt",
            "C:evil.dll",
        ] {
            let Ok(rel) = safe_rel_path(name) else {
                continue; // 被拒的条目根本不会被拼接
            };
            let joined = dest.join(&rel);
            assert!(
                joined.starts_with(dest),
                "{name:?} 拼出来跑到了目标目录外：{joined:?}"
            );
        }
    }

    #[test]
    fn zip_dir_skips_noise_and_roundtrips() {
        let d = tmp();
        let src = d.path().join("inst");
        std::fs::create_dir_all(src.join(".minecraft/mods")).unwrap();
        std::fs::write(src.join("instance.json"), br#"{"slug":"inst"}"#).unwrap();
        std::fs::write(src.join(".minecraft/mods/a.jar"), b"mod").unwrap();
        std::fs::create_dir_all(src.join(".minecraft/saves/w")).unwrap();
        std::fs::write(src.join(".minecraft/saves/w/level.dat"), b"world").unwrap();
        // 噪音：都不该进包
        std::fs::create_dir_all(src.join("natives")).unwrap();
        std::fs::write(src.join("natives/lwjgl.dll"), b"x").unwrap();
        std::fs::create_dir_all(src.join(".minecraft/logs")).unwrap();
        std::fs::write(src.join(".minecraft/logs/latest.log"), b"noise").unwrap();
        std::fs::write(src.join(".minecraft/hs_err_pid1.log"), b"noise").unwrap();

        let zip_path = d.path().join("out/inst.zip");
        let summary = zip_dir(&src, &zip_path, &[]).unwrap();
        assert_eq!(summary.files, 3, "instance.json + a.jar + level.dat");
        assert!(summary.bytes > 0);
        assert!(zip_path.is_file());

        let dst = d.path().join("restored");
        std::fs::create_dir_all(&dst).unwrap();
        let n = unzip_to(&zip_path, &dst).unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            std::fs::read(dst.join(".minecraft/mods/a.jar")).unwrap(),
            b"mod"
        );
        assert_eq!(
            std::fs::read(dst.join(".minecraft/saves/w/level.dat")).unwrap(),
            b"world"
        );
        assert!(!dst.join("natives").exists());
        assert!(!dst.join(".minecraft/logs").exists());
    }

    #[test]
    fn zip_dir_can_exclude_extra_directories() {
        let d = tmp();
        let src = d.path().join("inst");
        std::fs::create_dir_all(src.join(".minecraft/saves/w")).unwrap();
        std::fs::write(src.join("instance.json"), b"{}").unwrap();
        std::fs::write(src.join(".minecraft/saves/w/level.dat"), b"world").unwrap();
        std::fs::create_dir_all(src.join(".minecraft/mods")).unwrap();
        std::fs::write(src.join(".minecraft/mods/a.jar"), b"mod").unwrap();

        // 分享实例但不带自己的存档（存档可能几个 GB，而且是别人的世界）
        let zip_path = d.path().join("share.zip");
        let s = zip_dir(&src, &zip_path, &["saves"]).unwrap();
        assert_eq!(s.files, 2, "instance.json + a.jar");

        let dst = d.path().join("restored");
        std::fs::create_dir_all(&dst).unwrap();
        unzip_to(&zip_path, &dst).unwrap();
        assert!(!dst.join(".minecraft/saves").exists());
        assert!(dst.join(".minecraft/mods/a.jar").is_file());
    }

    #[test]
    fn unzip_refuses_to_write_outside_the_destination() {
        let d = tmp();
        // 手工造一个带 ../ 条目的 zip（这正是"别人给的压缩包"）
        let evil = d.path().join("evil.zip");
        {
            let f = File::create(&evil).unwrap();
            let mut zip = zip::ZipWriter::new(f);
            let opts: zip::write::SimpleFileOptions = Default::default();
            zip.start_file("../pwned.txt", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"pwned").unwrap();
            zip.start_file("C:\\Windows\\pwned2.txt", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"pwned").unwrap();
            zip.finish().unwrap();
        }
        let dst = d.path().join("out");
        std::fs::create_dir_all(&dst).unwrap();
        let err = unzip_to(&evil, &dst).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
        assert!(!d.path().join("pwned.txt").exists());
    }

    #[test]
    fn read_text_entry_does_not_extract_the_whole_archive() {
        let d = tmp();
        let src = d.path().join("inst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("instance.json"), br#"{"name":"Pack"}"#).unwrap();
        std::fs::write(src.join("big.bin"), vec![0u8; 4 * 1024 * 1024]).unwrap();
        let zip_path = d.path().join("a.zip");
        zip_dir(&src, &zip_path, &[]).unwrap();

        let got = read_text_entry(&zip_path, "instance.json")
            .unwrap()
            .unwrap();
        assert!(got.contains("Pack"), "{got}");
        assert!(read_text_entry(&zip_path, "nope.json").unwrap().is_none());

        // 过大的"配置文件"直接拒绝，而不是读进内存
        let err = read_text_entry(&zip_path, "big.bin").unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
    }
}
