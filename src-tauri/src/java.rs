//! Java 运行时探测：按版本 JSON 的 `javaVersion.majorVersion` 找一个**真的能用**的 java。
//!
//! 关键规则：一律用 `java -version` 实测，不信路径名、不信注册表显示名、不信 `JAVA_HOME`
//! 的存在（它可能指向 JRE-only、指向已被卸载的目录、或指向与名字不符的版本）。
//! 探测失败就是失败，绝不"猜一个然后让游戏崩在用户面前"。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaRuntime {
    pub path: PathBuf,
    /// 大版本号（8 / 17 / 21 …）。
    pub version: u32,
    pub is_64bit: bool,
}

/// 从 `java -version` 的输出解析大版本号与位数。
///
/// 覆盖现实里出现过的几种写法：
/// - `openjdk version "21.0.4" 2024-07-16` → 21
/// - `java version "1.8.0_312"` → 8（Java 8 及更早用 1.x 编号，第二段才是大版本）
/// - `openjdk version "17.0.1" 2021-10-19` → 17
/// - 没有引号的 `openjdk 21.0.4 2024-07-16` → 21
/// - `... OpenJDK 64-Bit Server VM ...` → 64 位
pub fn parse_java_version(output: &str) -> Option<(u32, bool)> {
    let is_64bit = output.contains("64-Bit") || output.contains("64-bit");
    let candidate = quoted(output).or_else(|| first_version_like_token(output))?;
    let mut parts = candidate.split(['.', '-', '+', '_']);
    let first: u32 = parts.next()?.parse().ok()?;
    // 1.x 编号：1.8.0_312 → 8
    let major = if first == 1 {
        parts.next()?.parse().ok()?
    } else {
        first
    };
    if major == 0 {
        return None;
    }
    Some((major, is_64bit))
}

fn quoted(output: &str) -> Option<String> {
    let start = output.find('"')? + 1;
    let end = output[start..].find('"')? + start;
    let inner = &output[start..end];
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_string())
}

/// 没有引号时，找第一个"看起来像版本号"的 token（数字开头、含点、可带 -/+ 后缀）。
fn first_version_like_token(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|tok| {
        let t = tok.trim_end_matches([',', ':']);
        let ok = t.starts_with(|c: char| c.is_ascii_digit())
            && t.contains('.')
            && t.split(['.', '-', '+'])
                .take(2)
                .all(|p| p.parse::<u32>().is_ok());
        ok.then(|| t.to_string())
    })
}

/// 候选 java 路径，按可信度排序：JAVA_HOME → 我们自己下载的 JRE → PATH → 各平台常见安装位置。
///
/// 顺序有意义：`JAVA_HOME` 是用户明确表达的意图；我们下载的 JRE 版本可控；
/// PATH 上的可能是个 8 位的古董；扫描到的安装目录最不可控，放最后。
pub fn candidates(our_java_root: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("JAVA_HOME")
        && !home.is_empty()
    {
        out.push(java_in(Path::new(&home)));
    }
    if let Some(root) = our_java_root {
        out.extend(scan_one_level(root, |_| true).map(|dir| java_in(&dir)));
    }
    out.push(PathBuf::from(java_exe_name()));
    out.extend(platform_candidates());
    out
}

fn java_exe_name() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

/// `<home>/bin/java[.exe]`
fn java_in(home: &Path) -> PathBuf {
    home.join("bin").join(java_exe_name())
}

/// 扫一层子目录（`C:\Program Files\Java\*`、`/usr/lib/jvm/*` 这类布局），过滤掉隐藏项。
fn scan_one_level(root: &Path, keep: impl Fn(&str) -> bool) -> impl Iterator<Item = PathBuf> {
    let entries = std::fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !n.starts_with('.') && keep(n))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    entries.into_iter()
}

fn platform_candidates() -> Vec<PathBuf> {
    let roots: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files\Java",
            r"C:\Program Files\Eclipse Adoptium",
            r"C:\Program Files\Microsoft",
            r"C:\Program Files\Zulu",
            r"C:\Program Files\Amazon Corretto",
            r"C:\Program Files (x86)\Java",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/Library/Java/JavaVirtualMachines",
            "/System/Library/Java/JavaVirtualMachines",
            "/opt/homebrew/opt",
            "/usr/local/opt",
        ]
    } else {
        &["/usr/lib/jvm", "/usr/java", "/opt/java", "/usr/lib64/jvm"]
    };

    let mut out = Vec::new();
    for root in roots {
        let root = Path::new(root);
        // macOS 的 JVM 布局多一层 Contents/Home；Homebrew 的 openjdk 还多一层 libexec/openjdk.jdk
        for dir in scan_one_level(root, |_| true) {
            if cfg!(target_os = "macos") {
                let home = dir.join("Contents/Home");
                if home.is_dir() {
                    out.push(java_in(&home));
                }
                let nested = dir.join("libexec/openjdk.jdk/Contents/Home");
                if nested.is_dir() {
                    out.push(java_in(&nested));
                }
            } else {
                out.push(java_in(&dir));
            }
        }
    }
    // 用户级安装（不需要管理员权限的那些）
    if let Some(local) = user_local_java_roots() {
        for root in local {
            out.extend(scan_one_level(&root, |_| true).map(|d| java_in(&d)));
        }
    }
    out
}

fn user_local_java_roots() -> Option<Vec<PathBuf>> {
    let home = std::env::var(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    })
    .ok()?;
    let home = PathBuf::from(home);
    Some(if cfg!(windows) {
        vec![
            home.join("Programs/Eclipse Adoptium"),
            home.join("Programs/Java"),
            home.join("Programs/Microsoft"),
        ]
    } else if cfg!(target_os = "macos") {
        vec![home.join("Library/Java/JavaVirtualMachines")]
    } else {
        vec![home.join(".sdkman/candidates/java"), home.join(".jdks")]
    })
}

/// 实测一个 java 可执行文件的版本。`java -version` 的输出在 **stderr**（历史包袱），
/// 两处都读一遍更稳妥。
pub async fn probe(path: &Path) -> Option<JavaRuntime> {
    // -Xshare:off 避免某些 JDK 在无写权限目录时报警告；-version 不会真的启动 JVM
    let out = Command::new(path).arg("-version").output().await.ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let (version, is_64bit) = parse_java_version(&text)?;
    Some(JavaRuntime {
        path: path.to_path_buf(),
        version,
        is_64bit,
    })
}

/// 找一个满足 major 的运行时。`exact` 为真时只接受完全相同的大版本
/// （1.16 及以下必须用 Java 8：新 JDK 跑不了老版本的 LWJGL），否则接受 >= major。
pub async fn find(major: u32, exact: bool, our_java_root: Option<&Path>) -> Option<JavaRuntime> {
    for candidate in candidates(our_java_root) {
        let Some(found) = probe(&candidate).await else {
            continue;
        };
        let ok = if exact {
            found.version == major
        } else {
            found.version >= major
        };
        if ok {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_java_version_outputs() {
        let cases = [
            // Oracle/OpenJDK 现代版本
            (
                "openjdk version \"21.0.4\" 2024-07-16\nOpenJDK Runtime Environment Temurin-21.0.4+7 (build 21.0.4+7)\nOpenJDK 64-Bit Server VM Temurin-21.0.4+7 (build 21.0.4+7, mixed mode, sharing)",
                Some((21, true)),
            ),
            (
                "openjdk version \"17.0.1\" 2021-10-19\nOpenJDK Runtime Environment (build 17.0.1+12)\nOpenJDK 64-Bit Server VM (build 17.0.1+12, mixed mode)",
                Some((17, true)),
            ),
            // Java 8：1.x 编号
            (
                "java version \"1.8.0_312\"\nJava(TM) SE Runtime Environment (build 1.8.0_312-b07)\nJava HotSpot(TM) 64-Bit Server VM (build 25.312-b07, mixed mode)",
                Some((8, true)),
            ),
            (
                "openjdk version \"1.8.0_402\"\nOpenJDK Runtime Environment (build 1.8.0_402-b06)\nOpenJDK 32-Bit Server VM (build 25.402-b06, mixed mode)",
                Some((8, false)),
            ),
            // 没有引号的输出
            (
                "openjdk 21.0.4 2024-07-16\nOpenJDK Runtime Environment Homebrew (build 21.0.4)\nOpenJDK 64-Bit Server VM Homebrew (build 21.0.4, mixed mode, sharing)",
                Some((21, true)),
            ),
            // 早期访问版
            (
                "openjdk version \"22-ea\" 2023-12-21\nOpenJDK 64-Bit Server VM (build 22-ea+11, mixed mode)",
                Some((22, true)),
            ),
            // Microsoft Build of OpenJDK
            (
                "openjdk version \"17.0.9\" 2023-10-17 LTS\nMicrosoft OpenJDK 17.0.9+8 (build 17.0.9+8)\nOpenJDK 64-Bit Server VM",
                Some((17, true)),
            ),
            // 垃圾输入
            ("", None),
            ("command not found", None),
            ("openjdk version \"\"", None),
        ];
        for (input, want) in cases {
            assert_eq!(
                parse_java_version(input),
                want,
                "input={:?}",
                input.lines().next().unwrap_or("")
            );
        }
    }

    #[test]
    fn java_home_comes_first_and_path_is_always_a_candidate() {
        let c = candidates(None);
        assert!(!c.is_empty());
        assert!(
            c.iter()
                .any(|p| p.file_name().is_some_and(|n| n == java_exe_name()))
        );
        if let Ok(home) = std::env::var("JAVA_HOME")
            && !home.is_empty()
        {
            assert_eq!(c[0], java_in(Path::new(&home)), "JAVA_HOME should be first");
        }
    }

    #[test]
    fn our_jre_root_is_scanned_before_path_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let jdk = dir.path().join("jdk-21");
        std::fs::create_dir_all(jdk.join("bin")).unwrap();
        std::fs::write(jdk.join("bin").join(java_exe_name()), b"#!/bin/sh\n").unwrap();
        let c = candidates(Some(dir.path()));
        let ours = java_in(&jdk);
        let plain = PathBuf::from(java_exe_name());
        let ours_idx = c
            .iter()
            .position(|p| *p == ours)
            .expect("our jre should be listed");
        let plain_idx = c
            .iter()
            .position(|p| *p == plain)
            .expect("PATH java should be listed");
        assert!(
            ours_idx < plain_idx,
            "our JRE must be preferred over PATH: {c:?}"
        );
        // 隐藏目录不当候选（.cache 之类）
        let hidden = dir.path().join(".hidden");
        std::fs::create_dir_all(hidden.join("bin")).unwrap();
        assert!(!c.iter().any(|p| p.starts_with(&hidden)));
    }

    #[tokio::test]
    async fn probe_of_a_non_java_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join(java_exe_name());
        std::fs::write(&fake, b"not a jvm").unwrap();
        // 不存在/不可执行的路径也必须安静返回 None（候选扫描本来就会扫到一堆不存在的路径）
        assert!(probe(&fake).await.is_none());
        assert!(probe(&dir.path().join("nope")).await.is_none());
    }
}
