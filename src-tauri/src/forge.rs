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
    let mut outputs = Vec::new();
    if let Some(map) = &p.outputs {
        for (k, sha1) in map {
            let resolved = substitute(std::slice::from_ref(k), vars, paths)?;
            let path = PathBuf::from(&resolved[0]);
            // 相对路径按 ROOT 解释（profile 里的 outputs 键实测是 {VAR} 形式）
            let path = if path.is_absolute() {
                path
            } else {
                paths.cache().join(path)
            };
            outputs.push((path, sha1.clone()));
        }
    }
    Ok(ProcessorRun {
        jar,
        classpath,
        main_class,
        args,
        outputs,
    })
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
}
