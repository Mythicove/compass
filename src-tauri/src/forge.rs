//! Forge 1.13+ 与 NeoForge 的 installer：`install_profile.json` + processor pipeline。
//!
//! 这是整个计划里技术风险最高的一块，所以每一步都对着**真的 installer 产物**核过
//! （样本：NeoForge 21.4.161 / Minecraft 1.21.4，`tests/fixtures/neoforge-install_profile.json`
//! 是官方 installer jar 里那份 130KB 的原文，未裁剪）。
//!
//! 流程（客户端）：
//! 1. 父版本（原版）必须已装好——processor 要拿 `{MINECRAFT_JAR}` 当输入；
//! 2. 下载 `install_profile.libraries`（70 条，形状与 Mojang 的库一致，含全部 processor 工具）；
//! 3. 解析 `data` 变量（三种形态，见 `DataValue`）+ 内建变量；
//! 4. 按顺序跑 `sides` 命中 client 的 processor：`java -cp <解析出的 classpath> <Main-Class> <替换后的参数>`，
//!    主类从 processor jar 自己的 `META-INF/MANIFEST.MF` 读；
//! 5. 最后把 installer 里的子版本 JSON 交给 `Installer::install_profile`（合并继承 + 下载库 + 落盘）。
//!
//! 启动时 classpath 里**不放任何游戏 jar**：`fml_loader` 自己按坐标去 libraries 目录找
//! 打过补丁的那个，启动器再塞一个就撞自动模块名（详见 `version::loader_provides_game_jar`）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::archive;
use crate::error::Error;
use crate::net::{Downloader, FileTask, ProgressSink};
use crate::paths::Paths;
use crate::version::{self, Library, VersionJson};

/// installer 里那份 `install_profile.json`。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallProfile {
    #[serde(default)]
    pub spec: i32,
    /// 子版本 JSON 在 installer jar 里的路径（实测 `/version.json`，带前导斜杠）
    pub json: String,
    /// 目标游戏版本
    pub minecraft: String,
    #[serde(default)]
    pub data: HashMap<String, SideValue>,
    #[serde(default)]
    pub processors: Vec<Processor>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    /// 服务端 jar 的落点（客户端安装用不到，但字段存在）
    #[serde(default)]
    pub server_jar_path: String,
}

/// 一个 data 变量的两侧取值。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SideValue {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub server: String,
}

impl SideValue {
    pub fn for_side(&self, side: &str) -> &str {
        if side == "server" {
            &self.server
        } else {
            &self.client
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Processor {
    /// 空 = 两侧都跑。实测 NeoForge 21.4.161 的 10 个 processor 里
    /// 4 个是 `["server"]`、1 个 `["client"]`、5 个没有这个字段。
    #[serde(default)]
    pub sides: Vec<String>,
    /// processor 自己的 Maven 坐标（主类从它的 MANIFEST 读）
    pub jar: String,
    #[serde(default)]
    pub classpath: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// 产物校验（`{"{PATCHED}": "sha1…"}`）。**NeoForge 21.4.161 的 10 个 processor
    /// 一个都没有这个字段**，部分 Forge 版本有；所以按"有就校验、没有就跳过"处理。
    #[serde(default)]
    pub outputs: Option<HashMap<String, String>>,
}

impl Processor {
    pub fn runs_on(&self, side: &str) -> bool {
        self.sides.is_empty() || self.sides.iter().any(|s| s == side)
    }
}

/// `data` 值的三种形态（实测三种都出现在同一份 profile 里）。
#[derive(Debug, Clone, PartialEq)]
pub enum DataValue {
    /// `[group:artifact:version[:classifier][@ext]]` → libraries 下的路径
    Maven(String),
    /// `/data/client.lzma` → installer jar **内部**的条目，要先解出来
    InstallerEntry(String),
    /// `'1.21.4-20241203.161809'` → 字面量（去掉引号）
    Literal(String),
}

/// 解析一个 data 值。认不出的形态**报错**而不是当字面量：
/// 静默当字面量的话，`[…]` 会被当成路径传给 processor，报错会出现在 Java 那边，
/// 而栈里看不出是我们的解析漏了一种形态。
pub fn parse_data_value(raw: &str) -> Result<DataValue, Error> {
    let t = raw.trim();
    if let Some(inner) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        if inner.is_empty() {
            return Err(Error::Invalid(
                "empty maven coordinate in install profile data".into(),
            ));
        }
        return Ok(DataValue::Maven(inner.to_string()));
    }
    if let Some(rest) = t.strip_prefix('/') {
        if rest.is_empty() {
            return Err(Error::Invalid(
                "empty installer entry in install profile data".into(),
            ));
        }
        return Ok(DataValue::InstallerEntry(rest.to_string()));
    }
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        return Ok(DataValue::Literal(t[1..t.len() - 1].to_string()));
    }
    Err(Error::Invalid(format!(
        "unrecognized install profile data value: {raw:?} (期待 […]/…/'…' 三种形态之一)"
    )))
}

/// Maven 坐标 → libraries 下的绝对路径。
pub fn library_path(paths: &Paths, coord: &str) -> Result<PathBuf, Error> {
    let rel = version::maven_path(coord).ok_or_else(|| {
        Error::Invalid(format!(
            "unusable maven coordinate in install profile: {coord:?}"
        ))
    })?;
    Ok(paths.libraries().join(rel))
}

/// 替换 processor 参数里的两种 token：
///
/// - `{VAR}`：内建变量与 data 变量；
/// - `[group:artifact:version[:classifier][@ext]]`：**内联的 Maven 坐标** → libraries 下的路径。
///   实测 NeoForge 的 MCP_DATA 就是 `--input [net.neoforged:neoform:1.21.4-20241203.161809@zip]`；
///   不解析的话 installertools 会报 `Input does not exist: [net.neoforged:…]`——
///   这是真跑出来的第一个失败，报错信息里带着没被替换的原文。
///
/// 未知变量与不配对的括号都**报错**：漏一个的话 processor 会拿到字面量 `{X}` 当路径，
/// 然后在 Java 那边报一个看不出根因的错。
pub fn substitute(
    args: &[String],
    vars: &HashMap<String, String>,
    paths: &Paths,
) -> Result<Vec<String>, Error> {
    args.iter()
        .map(|a| substitute_one(a, vars, paths))
        .collect()
}

fn substitute_one(
    arg: &str,
    vars: &HashMap<String, String>,
    paths: &Paths,
) -> Result<String, Error> {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    loop {
        // 最近的一个 token 起点：`{` 是变量，`[` 是内联 Maven 坐标
        let next = match (rest.find('{'), rest.find('[')) {
            (Some(v), Some(c)) => Some(if v <= c { (v, true) } else { (c, false) }),
            (Some(v), None) => Some((v, true)),
            (None, Some(c)) => Some((c, false)),
            (None, None) => None,
        };
        let Some((idx, is_var)) = next else {
            out.push_str(rest);
            return Ok(out);
        };
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 1..];
        let close = if is_var { '}' } else { ']' };
        let end = after.find(close).ok_or_else(|| {
            Error::Invalid(format!("unbalanced {close} in processor argument: {arg:?}"))
        })?;
        let body = &after[..end];
        let value = if is_var {
            vars.get(body)
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "unknown variable {{{body}}} in processor argument: {arg:?}"
                    ))
                })?
                .clone()
        } else {
            library_path(paths, body)?.to_string_lossy().into_owned()
        };
        out.push_str(&value);
        rest = &after[end + 1..];
    }
}

/// 一个 processor 展开成可执行的东西。
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessorRun {
    pub jar: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub main_class: String,
    pub args: Vec<String>,
    /// 产物路径 → 期望 sha1
    pub outputs: Vec<(PathBuf, String)>,
}

/// 展开一个 processor：解析坐标、读主类、替换参数。
///
/// 主类从 processor jar 自己的 MANIFEST 读（`Main-Class`），不写死：
/// installertools / jarsplitter / AutoRenamingTool / binarypatcher 各有各的主类，
/// 而且随版本变。
pub fn plan_processor(
    p: &Processor,
    vars: &HashMap<String, String>,
    paths: &Paths,
) -> Result<ProcessorRun, Error> {
    let jar = library_path(paths, &p.jar)?;
    if !jar.is_file() {
        return Err(Error::Config(format!(
            "processor jar missing: {} (install profile 的库没下全？)",
            jar.display()
        )));
    }
    let main_class = archive::manifest_attr(&jar, "Main-Class")?.ok_or_else(|| {
        Error::Config(format!(
            "{} has no Main-Class in its manifest",
            jar.display()
        ))
    })?;

    // jar 自己要在 classpath 上。实测 profile 里它通常也已经列在 classpath 中
    // （installertools 就是），所以这里只在缺失时补，避免重复。
    let mut classpath = vec![jar.clone()];
    for coord in &p.classpath {
        let path = library_path(paths, coord)?;
        if path != jar && !classpath.contains(&path) {
            classpath.push(path);
        }
    }

    let args = substitute(&p.args, vars, paths)?;
    let outputs = resolve_outputs(p, vars, paths)?;
    Ok(ProcessorRun {
        jar,
        classpath,
        main_class,
        args,
        outputs,
    })
}

/// 把 profile 里的 `outputs` 解析成（产物路径, 期望 sha1）。
///
/// 键与值**都是** `{VAR}` 形式：键指向产物路径，值指向 data 里的期望 sha1
/// （实测 Forge 1.21.4 写的是 `{MOJMAPS}={MOJMAPS_SHA}`）。只替换键的话，校验会拿
/// 字面量 `"{MOJMAPS_SHA}"` 去比对真 sha1——必然失败，而且报错长得像"产物被篡改"，
/// 把人往错误方向带。NeoForge 21.4 一个 `outputs` 都没有，所以这条路径只有真装
/// Forge 才走得到；单独抽成函数是为了能离线钉住它（`plan_processor` 要 jar 在磁盘上）。
pub fn resolve_outputs(
    p: &Processor,
    vars: &HashMap<String, String>,
    paths: &Paths,
) -> Result<Vec<(PathBuf, String)>, Error> {
    let Some(map) = &p.outputs else {
        return Ok(Vec::new());
    };
    let mut outputs = Vec::with_capacity(map.len());
    for (k, sha1) in map {
        let resolved = substitute(&[(*k).clone(), (*sha1).clone()], vars, paths)?;
        let path = PathBuf::from(&resolved[0]);
        // 相对路径按 ROOT 解释（profile 里的 outputs 键实测是 {VAR} 形式）
        let path = if path.is_absolute() {
            path
        } else {
            paths.cache().join(path)
        };
        outputs.push((path, resolved[1].clone()));
    }
    Ok(outputs)
}

/// data 里那些指向 **installer jar 内部**的条目（如 `/data/client.lzma`），先解到磁盘上。
/// 返回 data 变量名 → 落点路径。
///
/// 单独一步是为了让 `resolve_vars` 保持纯函数：变量表算错了要能离线测出来，
/// 而不是只有真跑 installer 才知道。
pub fn extract_data_entries(
    profile: &InstallProfile,
    side: &str,
    jar: &Path,
    extracted: &Path,
) -> Result<HashMap<String, PathBuf>, Error> {
    let mut out = HashMap::new();
    for (name, sv) in &profile.data {
        if let DataValue::InstallerEntry(entry) = parse_data_value(sv.for_side(side))? {
            // 落点用条目的文件名：不同 profile 的条目可能同名（client.lzma/server.lzma 不会，
            // 但别赌），所以按 data 变量名分目录，天然不撞
            let dest =
                extracted
                    .join(name)
                    .join(Path::new(&entry).file_name().ok_or_else(|| {
                        Error::Invalid(format!("unusable installer entry {entry:?}"))
                    })?);
            if !dest.is_file() {
                extract_entry(jar, &entry, &dest)?;
            }
            out.insert(name.clone(), dest);
        }
    }
    Ok(out)
}

/// 把 installer jar 里 `maven/` 那棵树解到 `libraries/`（返回解出的文件数）。
///
/// Forge 的版本 JSON 里**每个版本都恰好有一条 `"url": ""` 的库**（实测 1.12.2 /
/// 1.16.5 / 1.21.4 各一条）——那个产物不在 Maven 上，所以没有下载地址：
/// - 1.12.2 / 1.16.5：它打包在 installer 的 `maven/<maven 路径>` 里，官方 installer
///   就是从那儿拷的，这一步做的是同一件事；
/// - 1.21.4：它是 binarypatcher 的输出（`{PATCHED}`，sha1 与那条库声明的一致），
///   processor 先跑，文件已在位。
///
/// 对两种情况都安全：解出来的文件要么正是需要的，要么与 processor 的产物同路径同内容。
/// 校验也没有因此放松——版本 JSON 给每条库都带了 sha1，下载器认"已就位"靠的就是它，
/// 对不上的文件会被判为未完成并报错，不会被静默当成可用。
pub fn extract_installer_maven(jar: &Path, libraries: &Path) -> Result<usize, Error> {
    let file = std::fs::File::open(jar)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .filter(|n| n.starts_with("maven/") && !n.ends_with('/'))
        .collect();
    let mut count = 0usize;
    for name in names {
        let rel = name.strip_prefix("maven/").unwrap_or(&name);
        // 与解 zip 的其它路径一样过 zip-slip 检查：installer 是从 Maven 校验下来的，
        // 但"来源可信"不等于"里面的路径都规矩"，写出库目录之外的代价太大
        let rel = crate::archive::safe_rel_path(rel).map_err(|_| {
            Error::Invalid(format!(
                "refusing unsafe entry {name:?} in {}",
                jar.display()
            ))
        })?;
        let dest = libraries.join(&rel);
        if dest.is_file() {
            continue;
        }
        extract_entry(jar, &name, &dest)?;
        count += 1;
    }
    Ok(count)
}

/// 内建变量 + data 变量。键名取自实测的 processor 参数。
///
/// 纯函数：不读文件、不解压。`extracted` 由 `extract_data_entries` 先算好。
pub fn resolve_vars(
    profile: &InstallProfile,
    side: &str,
    paths: &Paths,
    installer_jar: &Path,
    minecraft_jar: &Path,
    java_home: &Path,
    extracted: &HashMap<String, PathBuf>,
) -> Result<HashMap<String, String>, Error> {
    let mut vars = HashMap::new();
    let mut put = |k: &str, v: String| {
        vars.insert(k.to_string(), v);
    };
    put("SIDE", side.to_string());
    put("INSTALLER", installer_jar.to_string_lossy().into_owned());
    // ROOT = 含 libraries/ 的那一层：官方 installer 把 processor 产物写到
    // `<root>/libraries/net/minecraft/client/…`，实测确认
    put("ROOT", paths.cache().to_string_lossy().into_owned());
    put(
        "LIBRARY_DIR",
        paths.libraries().to_string_lossy().into_owned(),
    );
    put("MINECRAFT_VERSION", profile.minecraft.clone());
    put(
        "MINECRAFT_JAR",
        minecraft_jar.to_string_lossy().into_owned(),
    );
    put("JAVA_HOME", java_home.to_string_lossy().into_owned());

    for (name, side_value) in &profile.data {
        let raw = side_value.for_side(side);
        match parse_data_value(raw)? {
            DataValue::Maven(coord) => put(
                name,
                library_path(paths, &coord)?.to_string_lossy().into_owned(),
            ),
            DataValue::InstallerEntry(_) => {
                let dest = extracted.get(name).ok_or_else(|| {
                    Error::Config(format!(
                        "data {name} points into the installer jar but was not extracted"
                    ))
                })?;
                put(name, dest.to_string_lossy().into_owned())
            }
            DataValue::Literal(text) => put(name, text),
        }
    }
    Ok(vars)
}

/// 从 installer jar 里解出一个条目（`data/client.lzma` 这类二进制补丁）。
fn extract_entry(jar: &Path, entry: &str, dest: &Path) -> Result<(), Error> {
    let file = std::fs::File::open(jar)?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    let mut src = archive.by_name(entry).map_err(|_| {
        Error::Config(format!(
            "{} is missing entry {entry:?} (installer 与 profile 不匹配？)",
            jar.display()
        ))
    })?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 先写 .part 再改名：processor 中断时不留半个补丁文件
    let part = crate::net::part_path(dest);
    let mut out = std::fs::File::create(&part)?;
    std::io::copy(&mut src, &mut out)?;
    drop(out);
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// profile 里的库 → 下载任务（形状与 Mojang 的库一致，所以直接复用 FileTask）。
pub fn library_tasks(profile: &InstallProfile, paths: &Paths) -> Result<Vec<FileTask>, Error> {
    let mut tasks = Vec::with_capacity(profile.libraries.len());
    for lib in &profile.libraries {
        let Some(artifact) = lib.downloads.as_ref().and_then(|d| d.artifact.as_ref()) else {
            // 没有 downloads 块的库要靠 url + 坐标推（Fabric 那种形态）；
            // Forge/NeoForge 的 profile 实测每条都有 downloads，缺了就是数据坏了
            return Err(Error::Config(format!(
                "install profile library {} has no downloads.artifact",
                lib.name
            )));
        };
        let dest = match &artifact.path {
            Some(p) if !p.is_empty() => paths.libraries().join(p),
            _ => library_path(paths, &lib.name)?,
        };
        tasks.push(FileTask {
            url: artifact.url.clone(),
            dest,
            sha1: artifact.sha1.clone(),
            size: artifact.size,
        });
    }
    Ok(tasks)
}

/// 跑一个 processor。stdout/stderr 都收下来：失败时用户需要看到 Java 那边说了什么，
/// 只报"退出码 1"等于没报。
pub async fn run_processor(java: &Path, run: &ProcessorRun) -> Result<(), Error> {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = run
        .classpath
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(sep);
    let output = tokio::process::Command::new(java)
        .arg("-cp")
        .arg(cp)
        .arg(&run.main_class)
        .args(&run.args)
        .output()
        .await
        .map_err(Error::Io)?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Config(format!(
            "processor {} failed ({}): {}",
            run.main_class,
            output.status,
            tail(&format!("{stdout}{stderr}"))
        )));
    }
    // 有 outputs 就校验：processor 说成功但产物不对，比失败更难查
    for (path, want) in &run.outputs {
        let got = crate::net::sha1_of_file(path).await.map_err(|_| {
            Error::Config(format!(
                "processor {} did not produce {}",
                run.main_class,
                path.display()
            ))
        })?;
        if !want.is_empty() && got != *want {
            return Err(Error::Checksum(format!(
                "{}: want {want} got {got}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// 报错里只留输出的尾巴：processor 的日志能有几千行，全塞进错误串没法看。
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    const KEEP: usize = 12;
    if lines.len() <= KEEP {
        return text.trim().to_string();
    }
    format!(
        "…（{} 行输出，只留最后 {KEEP} 行）\n{}",
        lines.len(),
        lines[lines.len() - KEEP..].join("\n")
    )
}

/// 从 installer jar 读出 profile 与子版本 JSON。
pub fn read_installer(jar: &Path) -> Result<(InstallProfile, VersionJson), Error> {
    let text = archive::read_text_entry(jar, "install_profile.json")?.ok_or_else(|| {
        Error::Invalid(format!(
            "{} has no install_profile.json (不是 Forge/NeoForge 的 installer？)",
            jar.display()
        ))
    })?;
    let profile: InstallProfile = serde_json::from_str(&text)
        .map_err(|e| Error::Config(format!("install_profile.json is not readable: {e}")))?;
    let entry = profile.json.trim_start_matches('/');
    let child_text = archive::read_text_entry(jar, entry)?.ok_or_else(|| {
        Error::Invalid(format!(
            "{} has no {entry:?} (profile 说子版本 JSON 在这儿)",
            jar.display()
        ))
    })?;
    let child = VersionJson::parse(&child_text)
        .map_err(|e| Error::Config(format!("{entry} is not readable: {e}")))?;
    Ok((profile, child))
}

/// 下载 installer 的库并跑完 client 侧的 processor。
///
/// 返回跑过的 processor 数（UI 报进度用）。**不**负责装子版本本身——
/// 那由调用方接着调 `Installer::install_profile`，这样"跑 processor"与
/// "装版本"两件事各自可测、也可分别重试。
pub async fn run_client_processors(
    paths: &Paths,
    dl: &Downloader,
    jar: &Path,
    profile: &InstallProfile,
    java: &Path,
    minecraft_jar: &Path,
    progress: Option<ProgressSink>,
) -> Result<usize, Error> {
    // 1. processor 工具本身要先就位
    let summary = dl
        .download_all(library_tasks(profile, paths)?, progress)
        .await;
    if !summary.failed.is_empty() {
        return Err(Error::Config(format!(
            "failed to download {} installer librar(y/ies): {}",
            summary.failed.len(),
            crate::net::format_failures(&summary.failed)
        )));
    }
    // 2. 变量表：先把 installer 内的条目解出来，再算变量（后者是纯函数）
    let extracted_dir = paths.cache().join("forge-extract");
    std::fs::create_dir_all(&extracted_dir)?;
    let extracted = extract_data_entries(profile, "client", jar, &extracted_dir)?;
    let java_home = java
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let vars = resolve_vars(
        profile,
        "client",
        paths,
        jar,
        minecraft_jar,
        &java_home,
        &extracted,
    )?;

    // 3. 按序跑 client 侧的 processor
    let mut ran = 0usize;
    for p in &profile.processors {
        if !p.runs_on("client") {
            continue;
        }
        let run = plan_processor(p, &vars, paths)?;
        run_processor(java, &run).await?;
        ran += 1;
    }
    Ok(ran)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 官方 installer jar 里那份 install_profile.json 的原文（130KB，未裁剪）。
    fn real_profile() -> InstallProfile {
        let text = include_str!("../tests/fixtures/neoforge-install_profile.json");
        serde_json::from_str(text).unwrap_or_else(|e| panic!("real profile should parse: {e}"))
    }

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::new(dir.path().join("data"));
        p.ensure().unwrap();
        (dir, p)
    }

    #[test]
    fn parses_the_real_install_profile() {
        let p = real_profile();
        assert_eq!(p.spec, 1);
        assert_eq!(p.json, "/version.json", "子版本 JSON 的条目名带前导斜杠");
        assert_eq!(p.minecraft, "1.21.4");
        assert_eq!(p.processors.len(), 10);
        assert_eq!(p.libraries.len(), 70);
        assert_eq!(p.data.len(), 10);
        assert!(
            p.server_jar_path.contains("{LIBRARY_DIR}"),
            "serverJarPath 里也有变量：{}",
            p.server_jar_path
        );
        // 每条库都带 downloads.artifact（所以能直接复用 FileTask）
        assert!(
            p.libraries.iter().all(|l| l
                .downloads
                .as_ref()
                .and_then(|d| d.artifact.as_ref())
                .is_some()),
            "profile 的库应当都有 downloads.artifact"
        );
        // 库列表里含全部 processor 工具，但不含 binarypatcher 的**产物**
        for tool in [
            "net.neoforged.installertools:installertools:2.1.2",
            "net.neoforged.installertools:jarsplitter:2.1.2",
            "net.neoforged:AutoRenamingTool:2.0.3:all",
            "net.neoforged.installertools:binarypatcher:2.1.2:fatjar",
        ] {
            assert!(
                p.libraries.iter().any(|l| l.name == tool),
                "{tool} 应当在 profile 的库里"
            );
        }
        assert!(
            p.libraries
                .iter()
                .any(|l| l.name == "net.neoforged:neoforge:21.4.161:universal"),
            "universal 要下载"
        );
        assert!(
            !p.libraries
                .iter()
                .any(|l| l.name == "net.neoforged:neoforge:21.4.161:client"),
            "client（= 补丁产物）不在下载列表里，它是 binarypatcher 写出来的"
        );
    }

    #[test]
    fn processor_sides_decide_what_a_client_install_runs() {
        let p = real_profile();
        let client: Vec<usize> = p
            .processors
            .iter()
            .enumerate()
            .filter(|(_, x)| x.runs_on("client"))
            .map(|(i, _)| i)
            .collect();
        // 实测：0/1/2/7 是 server-only（EXTRACT_FILES 与 BUNDLER_EXTRACT），6 是 client-only，
        // 3/4/5/8/9 没有 sides 字段 = 两侧都跑
        assert_eq!(client, vec![3, 4, 5, 6, 8, 9], "{client:?}");
        let server: Vec<usize> = p
            .processors
            .iter()
            .enumerate()
            .filter(|(_, x)| x.runs_on("server"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(server, vec![0, 1, 2, 3, 4, 5, 7, 8, 9], "{server:?}");
        // sides 为空 = 两侧都跑，不是"谁都不跑"
        assert!(p.processors[3].sides.is_empty());
        assert!(p.processors[3].runs_on("client") && p.processors[3].runs_on("server"));
        // NeoForge 21.4 一个 outputs 都没有：实现必须容忍它缺失
        assert!(
            p.processors.iter().all(|x| x.outputs.is_none()),
            "这份 profile 里没有 outputs，别假定一定有"
        );
    }

    #[test]
    fn data_values_come_in_three_shapes() {
        let p = real_profile();
        let get = |k: &str| p.data.get(k).unwrap().client.clone();

        // 形态一：Maven 坐标
        assert_eq!(
            parse_data_value(&get("PATCHED")).unwrap(),
            DataValue::Maven("net.neoforged:neoforge:21.4.161:client".into())
        );
        assert_eq!(
            parse_data_value(&get("MAPPINGS")).unwrap(),
            DataValue::Maven("net.neoforged:neoform:1.21.4-20241203.161809:mappings@txt".into())
        );
        // 形态二：installer jar 内的条目
        assert_eq!(
            parse_data_value(&get("BINPATCH")).unwrap(),
            DataValue::InstallerEntry("data/client.lzma".into())
        );
        // 形态三：单引号字面量
        assert_eq!(
            parse_data_value(&get("MCP_VERSION")).unwrap(),
            DataValue::Literal("1.21.4-20241203.161809".into())
        );
        // 每个 data 项都必须是这三种之一（漏一种就会在跑 processor 时才炸）
        for (name, sv) in &p.data {
            assert!(
                parse_data_value(&sv.client).is_ok(),
                "data.{name} 的 client 值认不出来：{:?}",
                sv.client
            );
        }
        // MOJMAPS 两侧不同：客户端要用 client 的 mappings
        assert!(p.data["MOJMAPS"].client.contains("net.minecraft:client"));
        assert!(p.data["MOJMAPS"].server.contains("net.minecraft:server"));
        assert_ne!(p.data["MOJMAPS"].client, p.data["MOJMAPS"].server);
    }

    #[test]
    fn unrecognized_data_values_are_an_error_not_a_silent_literal() {
        for bad in ["", "[]", "/", "plain/path", "\"quoted\""] {
            assert!(parse_data_value(bad).is_err(), "should reject {bad:?}");
        }
        assert_eq!(
            parse_data_value("'x'").unwrap(),
            DataValue::Literal("x".into())
        );
    }

    #[test]
    fn substitutes_variables_and_refuses_unknown_ones() {
        let (_d, p) = paths();
        let mut vars = HashMap::new();
        vars.insert("MC_SRG".to_string(), "/libs/client-srg.jar".to_string());
        vars.insert("PATCHED".to_string(), "/libs/patched.jar".to_string());
        vars.insert("BINPATCH".to_string(), "/tmp/client.lzma".to_string());

        // 真实的 binarypatcher 参数（processor 9）
        let args: Vec<String> = [
            "--clean",
            "{MC_SRG}",
            "--output",
            "{PATCHED}",
            "--apply",
            "{BINPATCH}",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            substitute(&args, &vars, &p).unwrap(),
            vec![
                "--clean",
                "/libs/client-srg.jar",
                "--output",
                "/libs/patched.jar",
                "--apply",
                "/tmp/client.lzma"
            ]
        );
        // 没有变量的参数原样保留
        assert_eq!(
            substitute(
                &["--task".to_string(), "EXTRACT_FILES".to_string()],
                &vars,
                &p
            )
            .unwrap(),
            vec!["--task", "EXTRACT_FILES"]
        );
        // 未知变量必须报错：漏一个变量时 processor 会拿到字面量 {X} 当路径，
        // 然后在 Java 那边报一个看不出根因的错
        let err =
            substitute(&["--input".to_string(), "{NOPE}".to_string()], &vars, &p).unwrap_err();
        assert!(err.to_string().contains("NOPE"), "{err}");
        // 不配对的括号也要报
        assert!(substitute(&["{X".to_string()], &vars, &p).is_err());
        assert!(substitute(&["[a:b:c".to_string()], &vars, &p).is_err());
    }

    #[test]
    fn substitutes_inline_maven_coordinates_in_arguments() {
        // 真实的 MCP_DATA 参数（processor 3）：--input 是**内联的 Maven 坐标**，
        // 不是 {VAR}。真装时这里没解析，installertools 报的就是
        // `Input does not exist: [net.neoforged:neoform:1.21.4-20241203.161809@zip]`
        let (_d, p) = paths();
        let vars = HashMap::new();
        let args: Vec<String> = [
            "--task",
            "MCP_DATA",
            "--input",
            "[net.neoforged:neoform:1.21.4-20241203.161809@zip]",
            "--output",
            "{MAPPINGS}",
            "--key",
            "mappings",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let err = substitute(&args, &vars, &p).unwrap_err();
        assert!(
            err.to_string().contains("MAPPINGS"),
            "缺的变量要点名：{err}"
        );

        let mut vars = HashMap::new();
        vars.insert("MAPPINGS".to_string(), "/libs/mappings.txt".to_string());
        let got = substitute(&args, &vars, &p).unwrap();
        assert_eq!(got[0], "--task");
        assert_eq!(got[2], "--input");
        let input = &got[3];
        assert!(
            input.ends_with("neoform-1.21.4-20241203.161809.zip"),
            "内联坐标要解析成库路径（含 @zip 扩展名覆盖）：{input}"
        );
        assert!(
            input.starts_with(&p.libraries().to_string_lossy().to_string()),
            "解析结果必须在 libraries 下：{input}"
        );
        assert!(!input.contains('[') && !input.contains(']'), "{input}");
        assert_eq!(got[5], "/libs/mappings.txt");
        // 一个参数里混着两种 token 也要都替换掉
        let mixed = substitute(
            &["{SIDE}-[a:b:1.0]".to_string()],
            &{
                let mut m = HashMap::new();
                m.insert("SIDE".to_string(), "client".to_string());
                m
            },
            &p,
        )
        .unwrap();
        assert!(mixed[0].starts_with("client-"), "{}", mixed[0]);
        assert!(mixed[0].ends_with("b-1.0.jar"), "{}", mixed[0]);
    }

    #[test]
    fn library_paths_follow_maven_layout_including_classifier_and_ext() {
        let (_d, p) = paths();
        let plain = library_path(&p, "net.neoforged.installertools:installertools:2.1.2").unwrap();
        assert!(
            plain.ends_with(
                "libraries/net/neoforged/installertools/installertools/2.1.2/installertools-2.1.2.jar"
                    .replace('/', std::path::MAIN_SEPARATOR_STR)
                    .as_str()
            ) || plain.to_string_lossy().contains("installertools-2.1.2.jar"),
            "{}",
            plain.display()
        );
        // classifier（:all / :fatjar）与扩展名覆盖（@zip）都要落到正确的路径上
        let all = library_path(&p, "net.neoforged:AutoRenamingTool:2.0.3:all").unwrap();
        assert!(
            all.to_string_lossy()
                .ends_with("AutoRenamingTool-2.0.3-all.jar"),
            "{}",
            all.display()
        );
        let zip = library_path(&p, "net.neoforged:neoform:1.21.4-20241203.161809@zip").unwrap();
        assert!(
            zip.to_string_lossy()
                .ends_with("neoform-1.21.4-20241203.161809.zip"),
            "{}",
            zip.display()
        );
    }

    #[test]
    fn library_tasks_carry_the_checksums_from_the_profile() {
        let (_d, p) = paths();
        let profile = real_profile();
        let tasks = library_tasks(&profile, &p).unwrap();
        assert_eq!(tasks.len(), 70);
        // 空 sha1 = 无从校验，不等于校验通过；这里每条都该有真的 sha1
        assert!(
            tasks.iter().all(|t| t.sha1.len() == 40),
            "profile 的库都带 sha1，缺了就是解析漏了字段"
        );
        assert!(tasks.iter().all(|t| t.size > 0));
        assert!(tasks.iter().all(|t| t.url.starts_with("https://")));
        // 目标路径在 libraries/ 下，不会跑出缓存目录
        for t in &tasks {
            assert!(
                t.dest.starts_with(p.libraries()),
                "{} 不在 libraries/ 下",
                t.dest.display()
            );
        }
    }

    #[test]
    fn vars_include_everything_the_real_processors_reference() {
        let (d, p) = paths();
        let profile = real_profile();
        // resolve_vars 是纯函数：installer 内的条目由调用方先解好再传进来。
        // 这里给一个假的落点，验的是"变量表算得全、算得对"。
        let mut extracted = HashMap::new();
        extracted.insert("BINPATCH".to_string(), d.path().join("extract/client.lzma"));
        let vars = resolve_vars(
            &profile,
            "client",
            &p,
            &d.path().join("installer.jar"),
            &d.path().join("1.21.4.jar"),
            &d.path().join("jdk"),
            &extracted,
        )
        .unwrap();
        for k in [
            "SIDE",
            "INSTALLER",
            "ROOT",
            "LIBRARY_DIR",
            "MINECRAFT_VERSION",
            "MINECRAFT_JAR",
            "JAVA_HOME",
            "PATCHED",
            "MC_SRG",
            "BINPATCH",
        ] {
            assert!(vars.contains_key(k), "缺少 {{{k}}}");
        }
        assert_eq!(vars["SIDE"], "client");
        assert_eq!(vars["MINECRAFT_VERSION"], "1.21.4");
        assert_eq!(vars["ROOT"], p.cache().to_string_lossy());
        assert_eq!(vars["LIBRARY_DIR"], p.libraries().to_string_lossy());
        // Maven 形态的变量指向 libraries 下的真实布局
        assert!(
            vars["PATCHED"].contains("neoforge-21.4.161-client.jar"),
            "{}",
            vars["PATCHED"]
        );
        assert!(
            vars["MAPPINGS"].ends_with("neoform-1.21.4-20241203.161809-mappings.txt"),
            "{}",
            vars["MAPPINGS"]
        );
        // 字面量形态原样给出
        assert_eq!(vars["MCP_VERSION"], "1.21.4-20241203.161809");
        // installer 内的条目用的是调用方给的落点
        assert_eq!(
            vars["BINPATCH"],
            d.path().join("extract/client.lzma").to_string_lossy()
        );

        // 少给一个 installer 条目就报错，不会静默变成空串
        let err = resolve_vars(
            &profile,
            "client",
            &p,
            &d.path().join("installer.jar"),
            &d.path().join("1.21.4.jar"),
            &d.path().join("jdk"),
            &HashMap::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("BINPATCH"), "{err}");
    }

    #[test]
    fn every_real_processor_argument_resolves() {
        // 真正的契约测试：把 10 个 processor 的参数全部替换一遍，
        // 不许出现未知变量。少认一个内建变量的话，这里会立刻报出来，
        // 而不是等到 Java 那边拿到 "{ROOT}/libraries" 这种字面量再炸。
        let (_d, paths) = paths();
        let profile = real_profile();
        let mut vars = HashMap::new();
        for k in [
            "SIDE",
            "INSTALLER",
            "ROOT",
            "LIBRARY_DIR",
            "MINECRAFT_VERSION",
            "MINECRAFT_JAR",
            "JAVA_HOME",
            "MAPPINGS",
            "MOJMAPS",
            "MERGED_MAPPINGS",
            "MC_UNPACKED",
            "MC_SLIM",
            "MC_EXTRA",
            "MC_SRG",
            "PATCHED",
            "BINPATCH",
            "MCP_VERSION",
        ] {
            vars.insert(k.to_string(), format!("/tmp/{k}"));
        }
        for (i, proc) in profile.processors.iter().enumerate() {
            let args = substitute(&proc.args, &vars, &paths)
                .unwrap_or_else(|e| panic!("processor {i} ({}) 的参数替换失败: {e}", proc.jar));
            assert!(
                !args.iter().any(|a| a.contains('{')),
                "processor {i} 的参数里还有没替换的变量：{args:?}"
            );
            assert!(!args.is_empty(), "processor {i} 没有参数？");
        }
    }

    // ---- Forge：三份真 installer 里的原文（未裁剪）----
    //
    // 选这三个版本是因为它们**分属三套格式**，实现里走三条不同路径：
    // - `1.21.4-54.1.22`：spec 1、7 个 processor（client 侧 3 个）、5 个带 outputs、
    //   主类 `net.minecraftforge.bootstrap.ForgeBootstrap`、`--launchTarget forge_client`
    // - `1.16.5-36.2.39`：spec 0、4 个 processor（都没有 sides = client 全跑）、
    //   参数里有内联 Maven 坐标、工具是 SpecialSource、主类 `cpw.mods.modlauncher.Launcher`
    // - `1.12.2-14.23.5.2864`：spec 0、**processors 是空数组**、data 也是空的、
    //   版本 JSON 用旧式 `minecraftArguments` + `--tweakClass`、主类 `launchwrapper.Launch`
    //
    // NeoForge 只覆盖了第一套的一半（它一个 outputs 都没有），所以这三份不是冗余样本。
    const FORGE_VERSIONS: [&str; 3] = ["1.21.4-54.1.22", "1.16.5-36.2.39", "1.12.2-14.23.5.2864"];

    fn forge_profile(v: &str) -> InstallProfile {
        let text = match v {
            "1.21.4-54.1.22" => {
                include_str!("../tests/fixtures/forge-1.21.4-54.1.22-install_profile.json")
            }
            "1.16.5-36.2.39" => {
                include_str!("../tests/fixtures/forge-1.16.5-36.2.39-install_profile.json")
            }
            _ => include_str!("../tests/fixtures/forge-1.12.2-14.23.5.2864-install_profile.json"),
        };
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("{v} 的 install_profile.json 应当能解析: {e}"))
    }

    fn forge_version(v: &str) -> VersionJson {
        let text = match v {
            "1.21.4-54.1.22" => include_str!("../tests/fixtures/forge-1.21.4-54.1.22-version.json"),
            "1.16.5-36.2.39" => include_str!("../tests/fixtures/forge-1.16.5-36.2.39-version.json"),
            _ => include_str!("../tests/fixtures/forge-1.12.2-14.23.5.2864-version.json"),
        };
        VersionJson::parse(text).unwrap_or_else(|e| panic!("{v} 的 version.json 应当能解析: {e}"))
    }

    #[test]
    fn forge_profiles_span_three_install_formats() {
        let modern = forge_profile("1.21.4-54.1.22");
        assert_eq!(modern.spec, 1);
        assert_eq!(modern.json, "/version.json");
        assert_eq!(modern.minecraft, "1.21.4");
        assert_eq!(modern.processors.len(), 7);
        assert_eq!(modern.libraries.len(), 21);
        // client 侧只跑 3 个：[3] DOWNLOAD_MOJMAPS、[5] ForgeAutoRenamingTool、[6] binarypatcher。
        // 其余 4 个是 server-only——其中 [0] 带 `--exec {ROOT}/run.sh`，
        // 客户端安装跑到它就是**执行一个脚本**，sides 过滤错会有安全后果，不只是慢。
        let client: Vec<usize> = modern
            .processors
            .iter()
            .enumerate()
            .filter(|(_, p)| p.runs_on("client"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(client, vec![3, 5, 6], "{client:?}");
        assert!(modern.processors[0].args.iter().any(|a| a == "--exec"));
        // NeoForge 21.4 一个 outputs 都没有，Forge 1.21.4 有 5 个：两边都不能假定
        assert_eq!(
            modern
                .processors
                .iter()
                .filter(|p| p.outputs.is_some())
                .count(),
            5
        );
        // `{PATCHED}` 就是稍后要启动的 jar，profile 同时给了期望 sha1
        assert_eq!(
            modern.data["PATCHED"].client,
            "[net.minecraftforge:forge:1.21.4-54.1.22:client]"
        );
        assert_eq!(
            parse_data_value(&modern.data["PATCHED_SHA"].client).unwrap(),
            DataValue::Literal("7e7d911be6deea351a1bbf77dda80fbc3e756eb1".into())
        );
        // 新扩展名：NeoForge 用 `@txt`，Forge 1.21.4 用 `@tsrg`（maven_path 认任意字母数字扩展名）
        assert_eq!(
            modern.data["MOJMAPS"].client,
            "[net.minecraft:client:1.21.4:mappings@tsrg]"
        );
        assert_eq!(
            version::maven_path("net.minecraft:client:1.21.4:mappings@tsrg").as_deref(),
            Some("net/minecraft/client/1.21.4/client-1.21.4-mappings.tsrg")
        );

        let mid = forge_profile("1.16.5-36.2.39");
        assert_eq!(mid.spec, 0);
        assert_eq!(mid.processors.len(), 4);
        assert!(
            mid.processors.iter().all(|p| p.sides.is_empty()),
            "1.16.5 没有 sides 字段 = client 全跑"
        );
        assert_eq!(mid.libraries.len(), 39);
        // 内联 Maven 坐标（不是 `{VAR}`）出现在参数里，substitute 必须认
        assert!(
            mid.processors[0]
                .args
                .iter()
                .any(|a| a == "[de.oceanlabs.mcp:mcp_config:1.16.5-20210115.111550@zip]")
        );
        // 工具链与 NeoForge 不同：SpecialSource 而不是 AutoRenamingTool
        assert!(
            mid.processors
                .iter()
                .any(|p| p.jar.starts_with("net.md-5:SpecialSource"))
        );

        let legacy = forge_profile("1.12.2-14.23.5.2864");
        assert_eq!(legacy.spec, 0);
        // 没有 processor：整个 pipeline 要能跳过，连 Java 都不必探测
        assert!(
            legacy.processors.is_empty(),
            "{:?}",
            legacy.processors.len()
        );
        assert!(legacy.data.is_empty());
        assert_eq!(legacy.libraries.len(), 2);
        assert_eq!(legacy.minecraft, "1.12.2");
    }

    #[test]
    fn forge_version_jsons_settle_the_game_jar_question_per_format() {
        // 判据是"游戏参数里有没有 --launchTarget"，三套格式给出的答案不同，
        // 而且都必须对：1.13+ 由 fml/modlauncher 自己按坐标找补丁 jar（放了就撞自动模块名），
        // 1.12.2 靠 launchwrapper + --tweakClass，**必须**有原版 jar 在 classpath 上。
        for v in ["1.21.4-54.1.22", "1.16.5-36.2.39"] {
            let j = forge_version(v);
            assert!(
                version::loader_provides_game_jar(&j),
                "{v} 有 --launchTarget，不该由我们放游戏 jar"
            );
            assert!(j.arguments.is_some(), "{v} 用 modern arguments");
        }
        let modern = forge_version("1.21.4-54.1.22");
        assert_eq!(modern.id, "1.21.4-forge-54.1.22");
        assert_eq!(modern.inherits_from.as_deref(), Some("1.21.4"));
        assert_eq!(
            modern.main_class.as_deref(),
            Some("net.minecraftforge.bootstrap.ForgeBootstrap")
        );
        assert_eq!(modern.libraries.len(), 49);
        // Forge 1.21.4 的 target 是 `forge_client`（下划线），NeoForge 是 `neoforgeclient`，
        // 1.16.5 是 `fmlclient`：判据认的是 `--launchTarget` 这个键，不是它的值
        assert!(
            modern
                .arguments
                .as_ref()
                .unwrap()
                .game
                .iter()
                .any(|a| matches!(a, crate::version::ArgItem::Plain(s) if s == "forge_client"))
        );

        let mid = forge_version("1.16.5-36.2.39");
        assert_eq!(
            mid.main_class.as_deref(),
            Some("cpw.mods.modlauncher.Launcher")
        );
        assert_eq!(mid.libraries.len(), 26);

        let legacy = forge_version("1.12.2-14.23.5.2864");
        assert_eq!(legacy.id, "1.12.2-forge-14.23.5.2864");
        assert_eq!(legacy.inherits_from.as_deref(), Some("1.12.2"));
        assert_eq!(
            legacy.main_class.as_deref(),
            Some("net.minecraft.launchwrapper.Launch")
        );
        assert_eq!(legacy.libraries.len(), 22);
        assert!(legacy.arguments.is_none(), "1.12.2 没有 modern arguments");
        let args = legacy.minecraft_arguments.as_deref().unwrap();
        assert!(args.contains("--tweakClass"), "{args}");
        assert!(
            args.contains("net.minecraftforge.fml.common.launcher.FMLTweaker"),
            "{args}"
        );
        assert!(
            !version::loader_provides_game_jar(&legacy),
            "launchwrapper 要原版 jar 在 classpath 上"
        );
        // universal jar 是普通库（不是 processor 产物），必须在版本 JSON 里
        assert!(
            legacy
                .libraries
                .iter()
                .any(|l| l.name == "net.minecraftforge:forge:1.12.2-14.23.5.2864")
        );
    }

    #[test]
    fn every_forge_processor_argument_substitutes() {
        // 变量表是**按 profile 的 data 生成**的，不是写死一张表：Forge 1.21.4 用了
        // NeoForge 没有的 MC_OFF / MOJMAPS_SHA / PATCHED_SHA，写死的表会漏。
        let (_dir, paths) = paths();
        for v in FORGE_VERSIONS {
            let profile = forge_profile(v);
            let mut vars: HashMap<String, String> = HashMap::new();
            for k in profile.data.keys() {
                vars.insert(k.clone(), format!("/tmp/{k}"));
                vars.insert(format!("{k}_SHA"), format!("/tmp/{k}_SHA"));
            }
            for k in [
                "SIDE",
                "INSTALLER",
                "ROOT",
                "LIBRARY_DIR",
                "MINECRAFT_VERSION",
                "MINECRAFT_JAR",
                "JAVA_HOME",
            ] {
                vars.insert(k.to_string(), format!("/tmp/{k}"));
            }
            for (i, proc) in profile.processors.iter().enumerate() {
                let args = substitute(&proc.args, &vars, &paths).unwrap_or_else(|e| {
                    panic!("Forge {v} processor {i} ({}) 的参数替换失败: {e}", proc.jar)
                });
                assert!(
                    !args.iter().any(|a| a.contains('{')),
                    "Forge {v} processor {i} 的参数里还有没替换的变量：{args:?}"
                );
                // 内联坐标要变成 libraries 下的路径，不能原样留给 Java
                assert!(
                    !args.iter().any(|a| a.starts_with('[') && a.ends_with(']')),
                    "Forge {v} processor {i} 的内联 Maven 坐标没被解析：{args:?}"
                );
            }
        }
    }

    #[test]
    fn installer_maven_tree_is_extracted_into_libraries() {
        // Forge 每个版本都恰好有一条 `"url": ""` 的库；1.12.2 / 1.16.5 的那个产物
        // 只存在于 installer 的 maven/ 树里。这一步现代版本用不到（1.21.4 的空 URL 库
        // 是 binarypatcher 的产物），所以 live 测试盖不住它——必须有离线测试盯着。
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("installer-src");
        let bundled = src.join("maven/net/minecraftforge/forge/1.12.2-x/forge-1.12.2-x.jar");
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        std::fs::write(&bundled, b"universal").unwrap();
        // maven/ 之外的条目不该被解出来（data/ 由 extract_data_entries 按 data 变量单独解）
        std::fs::create_dir_all(src.join("data")).unwrap();
        std::fs::write(src.join("data/client.lzma"), b"patches").unwrap();

        let jar = dir.path().join("installer.jar");
        crate::archive::zip_dir(&src, &jar, &[]).unwrap();

        let paths = Paths::new(dir.path().join("data"));
        paths.ensure().unwrap();
        let n = extract_installer_maven(&jar, &paths.libraries()).unwrap();
        assert_eq!(n, 1, "只该解出 maven/ 下面那一个文件");

        let out = paths.libraries().join(
            [
                "net",
                "minecraftforge",
                "forge",
                "1.12.2-x",
                "forge-1.12.2-x.jar",
            ]
            .join(std::path::MAIN_SEPARATOR_STR),
        );
        assert_eq!(
            std::fs::read(&out).unwrap(),
            b"universal",
            "{}",
            out.display()
        );
        assert!(
            !paths.libraries().join("data").exists(),
            "maven/ 之外的条目不该被解到库里"
        );

        // 幂等：已经在位的文件不该被重写。重写会覆盖掉 processor 刚产出的同路径文件，
        // 而两者的内容不一定相同（1.21.4 的 universal 在 maven/ 里，:client 是补丁产物）
        assert_eq!(
            extract_installer_maven(&jar, &paths.libraries()).unwrap(),
            0,
            "第二次不该再解任何文件"
        );
    }

    #[test]
    fn installer_maven_extraction_refuses_path_traversal() {
        // installer 是从 Maven 按 sha1 校验下来的，但"来源可信"不等于"条目名规矩"：
        // 写出 libraries/ 之外就是往用户机器上任意位置落文件
        let dir = tempfile::tempdir().unwrap();
        // 恶意 zip 自己不能放在穿越的目标路径上，否则断言检查的是测试自己造的文件
        std::fs::create_dir_all(dir.path().join("installer")).unwrap();
        let jar = dir.path().join("installer").join("evil.jar");
        let file = std::fs::File::create(&jar).unwrap();
        let mut zw = zip::ZipWriter::new(file);
        zw.start_file(
            "maven/../../evil.jar",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        std::io::Write::write_all(&mut zw, b"pwned").unwrap();
        zw.finish().unwrap();

        let paths = Paths::new(dir.path().join("data"));
        paths.ensure().unwrap();
        // 穿越路径必须被拒绝（unwrap_err：返回 Ok 就是要修的 bug）
        let err = extract_installer_maven(&jar, &paths.libraries()).unwrap_err();
        assert!(err.to_string().contains("refusing unsafe entry"), "{err}");
        assert!(
            !dir.path().join("evil.jar").exists(),
            "文件不该被写到库目录之外"
        );
    }

    #[test]
    fn processor_output_checksums_are_variables_too() {
        // 真装 Forge 1.21.4 时炸出来的：outputs 的**值**是 `{MOJMAPS_SHA}` 这样的变量引用，
        // 只替换键就会拿字面量去比对 sha1。症状是一句
        // `want {MOJMAPS_SHA} got c23f3418…`——看起来像产物被篡改，其实是我们的替换漏了一半。
        // NeoForge 21.4 一个 outputs 都没有，所以这条路只有真装 Forge 才走得到。
        let (_dir, paths) = paths();
        let profile = forge_profile("1.21.4-54.1.22");
        let extracted: HashMap<String, PathBuf> =
            [("BINPATCH".to_string(), PathBuf::from("/tmp/client.lzma"))].into();
        let vars = resolve_vars(
            &profile,
            "client",
            &paths,
            Path::new("/tmp/installer.jar"),
            Path::new("/tmp/1.21.4.jar"),
            Path::new("/tmp/jdk"),
            &extracted,
        )
        .unwrap();

        let mojmaps = profile
            .processors
            .iter()
            .find(|p| p.jar.contains("installertools") && p.runs_on("client"))
            .expect("DOWNLOAD_MOJMAPS 那一步");
        let out = resolve_outputs(mojmaps, &vars, &paths).unwrap();
        assert_eq!(out.len(), 1);
        assert!(
            out[0].0.ends_with("client-1.21.4-mappings.tsrg"),
            "{:?}",
            out[0].0
        );
        assert_eq!(
            out[0].1, "c23f3418d42c5c6b12b43c79ec56ba6b2621289f",
            "值必须替换成 data 里的真 sha1（单引号字面量要去掉引号）"
        );

        // binarypatcher 的产物就是稍后要启动的 jar，它的 sha1 同样来自 data
        let patcher = profile
            .processors
            .iter()
            .find(|p| p.jar.contains("binarypatcher"))
            .unwrap();
        let out = resolve_outputs(patcher, &vars, &paths).unwrap();
        assert!(
            out[0].0.ends_with("forge-1.21.4-54.1.22-client.jar"),
            "{:?}",
            out[0].0
        );
        assert_eq!(out[0].1, "7e7d911be6deea351a1bbf77dda80fbc3e756eb1");

        // 5 个带 outputs 的 processor 全都要解析成"绝对路径 + 40 位十六进制"，
        // 一个 `{` 都不能剩（剩下的话 run_processor 会拿它当期望值去比对）
        let mut total = 0;
        for p in profile.processors.iter().filter(|p| p.outputs.is_some()) {
            for (path, want) in resolve_outputs(p, &vars, &paths).unwrap() {
                assert!(path.is_absolute(), "{path:?}");
                assert_eq!(want.len(), 40, "{path:?} -> {want:?}");
                assert!(
                    want.chars().all(|c| c.is_ascii_hexdigit()),
                    "没替换干净：{want:?}"
                );
                total += 1;
            }
        }
        assert_eq!(total, 5, "这份 profile 有 5 个 processor 带 outputs");
    }
}
