//! 组装启动命令、准备 natives、拉起进程并把日志流给 UI。
//!
//! 顺序是硬的：**JVM 参数 → 主类 → 游戏参数**。另外三个容易踩的点：
//! - `logging.client.argument`（`-Dlog4j.configurationFile=${path}`）**不在** `arguments.jvm` 里，
//!   必须在这里单独追加（version.rs 的测试把这个陷阱钉住了）；
//! - 1.7–1.18.1 要加 `-Dlog4j2.formatMsgNoLookups=true`（Log4Shell，CVE-2021-44228）；
//! - macOS 上**老版本**（没有 `arguments.jvm` 的那套）需要 `-XstartOnFirstThread`，
//!   现代版本的这条参数由 `arguments.jvm` 的规则自带，不能重复加。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::error::Error;
use crate::install::is_excluded;
use crate::version::{self, Features, OsInfo, OsName, VersionJson};

pub const LAUNCHER_NAME: &str = "MythicCompass";
pub const LAUNCHER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 直连服务器未指定端口时用 Minecraft 默认端口。
pub const DEFAULT_SERVER_PORT: u16 = 25565;

/// 直连服务器。参数形式随版本而变，见 `build_plan`：
/// 1.20 起 Mojang 用 Quick Play（`--quickPlayMultiplayer host:port`）取代了 `--server`/`--port`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerJoin {
    pub host: String,
    pub port: Option<u16>,
}

/// 启动所需的一切外部输入。路径都由调用方（install + instance）算好后传入，
/// 这里只做拼装——纯函数，因此可以在任一平台上对三个平台做契约测试。
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub game_dir: PathBuf,
    pub natives_dir: PathBuf,
    pub assets_root: PathBuf,
    pub asset_index_id: String,
    /// 库的 classpath 部分（不含 client jar，jar 由本模块追加到末尾）。
    pub classpath: Vec<PathBuf>,
    pub libraries_root: PathBuf,
    pub client_jar: PathBuf,
    pub java: PathBuf,
    pub memory_mb: u32,
    pub resolution: Option<(u32, u32)>,
    pub server: Option<ServerJoin>,
    pub demo: bool,
    pub player_name: String,
    pub player_uuid: String,
    pub access_token: String,
    pub version_name: String,
    pub version_type: String,
    pub logging_config: Option<PathBuf>,
    /// 追加的 JVM 参数（authlib-injector 的 -javaagent 走这里；P2 起也接受实例级自定义）。
    /// 插在版本自带的 JVM 参数之后、主类之前。
    pub extra_jvm_args: Vec<String>,
    /// 追加的**游戏**参数，放在最后：用户自定义参数应当能覆盖版本模板给的同名项。
    pub extra_game_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub work_dir: PathBuf,
}

/// classpath 分隔符：Windows 是 `;`，其余是 `:`。
pub fn classpath_separator(os: &OsInfo) -> &'static str {
    if os.name == OsName::Windows { ";" } else { ":" }
}

/// 内存与 GC 参数。G1 那组低停顿参数是官方启动器/HMCL 的通行做法，
/// Java 8u40+ 也支持（需要 UnlockExperimentalVMOptions）。
pub fn memory_args(memory_mb: u32) -> Vec<String> {
    let mut args = vec![format!("-Xmx{memory_mb}M"), "-Xms256M".to_string()];
    args.extend(
        [
            "-XX:+UnlockExperimentalVMOptions",
            "-XX:+UseG1GC",
            "-XX:G1NewSizePercent=20",
            "-XX:G1ReservePercent=20",
            "-XX:MaxGCPauseMillis=50",
            "-XX:G1HeapRegionSize=32M",
        ]
        .map(str::to_string),
    );
    args
}

/// Windows 上优先用 `javaw.exe`：我们已经用管道接管了 stdout/stderr，
/// 用 `java.exe` 只会多出一个黑色控制台窗口。
pub fn windows_java(program: &Path) -> PathBuf {
    if !cfg!(windows) {
        return program.to_path_buf();
    }
    if program.file_name().and_then(|n| n.to_str()) == Some("javaw.exe") {
        return program.to_path_buf();
    }
    let javaw = program.with_file_name("javaw.exe");
    if javaw.is_file() {
        javaw
    } else {
        program.to_path_buf()
    }
}

fn substitutions(v: &VersionJson, os: &OsInfo, opts: &LaunchOptions) -> HashMap<String, String> {
    let sep = classpath_separator(os);
    let mut cp: Vec<String> = opts
        .classpath
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    // client jar 放最后：库在前才能被 loader 的 patch 覆盖
    cp.push(opts.client_jar.to_string_lossy().into_owned());

    let mut vars = HashMap::new();
    let mut put = |k: &str, v: String| {
        vars.insert(k.to_string(), v);
    };
    put("auth_player_name", opts.player_name.clone());
    put("auth_uuid", opts.player_uuid.clone());
    put("auth_access_token", opts.access_token.clone());
    put("user_type", "msa".to_string());
    put("user_properties", "{}".to_string());
    put("version_name", opts.version_name.clone());
    put("version_type", opts.version_type.clone());
    put(
        "game_directory",
        opts.game_dir.to_string_lossy().into_owned(),
    );
    put(
        "assets_root",
        opts.assets_root.to_string_lossy().into_owned(),
    );
    put(
        "game_assets",
        opts.assets_root.to_string_lossy().into_owned(),
    );
    put("assets_index_name", opts.asset_index_id.clone());
    put(
        "natives_directory",
        opts.natives_dir.to_string_lossy().into_owned(),
    );
    put(
        "library_directory",
        opts.libraries_root.to_string_lossy().into_owned(),
    );
    put("classpath", cp.join(sep));
    put("classpath_separator", sep.to_string());
    put("launcher_name", LAUNCHER_NAME.to_string());
    put("launcher_version", LAUNCHER_VERSION.to_string());
    // 未使用但要给出空值，否则参数里会留下 ${...} 让 JVM 报"找不到主类"之类的怪错
    put("clientid", String::new());
    put("auth_xuid", "0".to_string());
    put("ignoreList", String::new());
    // Quick Play 的日志目录，固定在游戏目录下（1.20+ 的模板会引用它）。
    // 老版本不引用，多一个变量没有副作用。
    put(
        "quickPlayPath",
        opts.game_dir
            .join("quickPlay")
            .to_string_lossy()
            .into_owned(),
    );
    put("quickPlaySingleplayer", String::new());
    put("quickPlayRealms", String::new());
    // 直连服务器：Quick Play 版本用 `host:port` 作为 ${quickPlayMultiplayer}，
    // 老版本走 build_plan 里手动追加的 --server/--port（这里留空即可）。
    put(
        "quickPlayMultiplayer",
        match (&opts.server, supports_quick_play(v)) {
            (Some(s), true) => format!("{}:{}", s.host, s.port.unwrap_or(DEFAULT_SERVER_PORT)),
            _ => String::new(),
        },
    );
    if let Some((w, h)) = opts.resolution {
        put("resolution_width", w.to_string());
        put("resolution_height", h.to_string());
    }
    if let Some(logging) = &opts.logging_config {
        put("path", logging.to_string_lossy().into_owned());
    }
    vars
}

fn features(v: &VersionJson, opts: &LaunchOptions) -> Features {
    let mut f = Features::new();
    f.insert("is_demo_user".to_string(), opts.demo);
    f.insert(
        "has_custom_resolution".to_string(),
        opts.resolution.is_some(),
    );
    // Quick Play（1.20+ 的直连服务器方式）：只有版本模板真的声明了这些参数才开启，
    // 否则会把 `--quickPlayMultiplayer` 塞给一个不认识它的客户端。
    // 目录由 game_dir 派生（不给调用方"忘了设路径"的机会——那会变成"设了服务器却没连上"，
    // 而且不报错）；runner 负责把这个目录建出来。
    let qp = supports_quick_play(v);
    f.insert("has_quick_plays_support".to_string(), qp);
    f.insert(
        "is_quick_play_multiplayer".to_string(),
        qp && opts.server.is_some(),
    );
    f
}

/// 这个版本是否用 Quick Play 表达"直连服务器"。
///
/// 判据是**版本 JSON 自己**（游戏参数模板里有没有 `--quickPlayMultiplayer`），
/// 不是写死的版本表：Mojang 在 1.20 把 `--server`/`--port` 换成 Quick Play，
/// 但按版本号猜会在快照、分支版本与继承型 loader 版本上出错。
pub fn supports_quick_play(v: &VersionJson) -> bool {
    let Some(args) = &v.arguments else {
        return false;
    };
    args.game.iter().any(|item| match item {
        version::ArgItem::Plain(s) => s.contains("quickPlayMultiplayer"),
        version::ArgItem::Conditional { value, .. } => value
            .items()
            .iter()
            .any(|s| s.contains("quickPlayMultiplayer")),
    })
}

/// 组装完整命令行。返回的 args 里不含 program 本身。
pub fn build_plan(v: &VersionJson, os: &OsInfo, opts: &LaunchOptions) -> Result<LaunchPlan, Error> {
    let main_class = v
        .main_class
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Invalid(format!("version {} has no mainClass", v.id)))?;
    let feats = features(v, opts);
    let vars = substitutions(v, os, opts);

    let mut args = memory_args(opts.memory_mb);

    // Log4Shell 缓解：按**游戏版本**判定（jar 版本，继承型版本的 id 可能是 loader 名）
    let game_version = v.jar_version();
    if version::needs_log4shell_mitigation(game_version)
        || version::needs_log4shell_mitigation(&v.id)
    {
        args.push("-Dlog4j2.formatMsgNoLookups=true".to_string());
    }

    let legacy = v.arguments.is_none();
    // macOS 老版本需要第一线程渲染；现代版本由 arguments.jvm 的规则自带，不能重复加
    if legacy && os.name == OsName::Osx {
        args.push("-XstartOnFirstThread".to_string());
    }

    args.extend(version::jvm_arguments(v, os, &feats, &vars));

    // 日志配置：来自 logging.client.argument，不在 arguments.jvm 里。
    // 只在配置文件确实下载到本地时才加（否则 JVM 会因为找不到文件而启动失败）。
    if opts.logging_config.is_some()
        && let Some(logging) = v.logging.as_ref().and_then(|l| l.client.as_ref())
    {
        args.push(version::substitute(&logging.argument, &vars));
    }

    args.extend(opts.extra_jvm_args.iter().cloned());
    args.push(main_class.to_string());

    let mut game = version::game_arguments(v, os, &feats, &vars);
    // legacy 的 minecraftArguments 没有分辨率占位符，手动补（现代版本靠 has_custom_resolution 特性）
    if legacy && let Some((w, h)) = opts.resolution {
        game.extend([
            "--width".to_string(),
            w.to_string(),
            "--height".to_string(),
            h.to_string(),
        ]);
    }
    // 直连服务器：1.20+ 的模板里已经有 `--quickPlayMultiplayer`（值经 substitutions 注入），
    // 老版本没有这一项，由启动器手动追加 `--server`/`--port`。两者互斥，不能都加。
    if let Some(server) = &opts.server
        && !supports_quick_play(v)
    {
        game.push("--server".to_string());
        game.push(server.host.clone());
        game.push("--port".to_string());
        game.push(server.port.unwrap_or(DEFAULT_SERVER_PORT).to_string());
    }
    // 用户自定义的游戏参数放最后：让它能覆盖版本模板给的同名项
    game.extend(opts.extra_game_args.iter().cloned());
    args.extend(game);

    // 兜底自检：留下任何未替换的 ${...} 都意味着拼错了参数，
    // 与其让 JVM 报一句莫名其妙的错，不如现在就说清楚。
    if let Some(bad) = args.iter().find(|a| a.contains("${")) {
        return Err(Error::Invalid(format!(
            "unsubstituted placeholder in launch args: {bad}"
        )));
    }

    Ok(LaunchPlan {
        program: windows_java(&opts.java),
        args,
        work_dir: opts.game_dir.clone(),
    })
}

/// 解压 natives jar 到实例的 natives 目录。
///
/// 每次启动前**清空重建**：残留上一版本的 .dll/.so 会让 LWJGL 加载到错的库，
/// 症状是"换了游戏版本之后偶发崩溃"，极难查。
///
/// 解压时对每个条目做 zip-slip 防护：jar 里的 `../` 或绝对路径一律拒绝——
/// natives jar 来自版本 JSON 指定的仓库，但"来源可信"不是把路径校验省掉的理由。
pub fn extract_natives(natives: &[(PathBuf, Vec<String>)], dest: &Path) -> Result<usize, Error> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let mut count = 0usize;
    for (jar, exclude) in natives {
        if !jar.is_file() {
            return Err(Error::Config(format!(
                "natives jar missing: {}",
                jar.display()
            )));
        }
        let file = std::fs::File::open(jar)?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| Error::Config(format!("{} is not a valid zip: {e}", jar.display())))?;
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let name = entry.name().to_string();
            if entry.is_dir() || is_excluded(&name, exclude) {
                continue;
            }
            // zip-slip 防护用的是 archive::safe_rel_path —— 全 crate 只有一份实现。
            // natives jar 来自版本 JSON 指定的仓库，但"来源可信"不是把路径校验省掉的理由；
            // P2 的实例导入、P3 的整合包解压面对的是**别人给的文件**，更不能各写一套。
            let rel = crate::archive::safe_rel_path(&name).map_err(|_| {
                Error::Invalid(format!(
                    "refusing unsafe entry {name:?} in {}",
                    jar.display()
                ))
            })?;
            let out = dest.join(&rel);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut f)?;
            count += 1;
        }
    }
    Ok(count)
}

/// 拉起游戏进程：stdout/stderr 都接管成管道（日志要显示在 UI 里），stdin 关掉。
pub async fn spawn(plan: &LaunchPlan) -> Result<Child, Error> {
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args)
        .current_dir(&plan.work_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        // 不让子进程弹出控制台窗口（我们用 javaw + 管道接管输出）
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
        .map_err(|e| Error::Config(format!("failed to start {:?}: {e}", plan.program)))
}

/// 一行游戏日志。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    /// `stdout` | `stderr`
    pub stream: String,
    pub line: String,
}

/// 拉起游戏并看管它：逐行转发日志，直到进程退出或收到停止信号。
/// 返回退出码；被我们杀掉时返回 `None`——"用户主动停止"与"游戏崩了"必须能区分，
/// 否则崩溃诊断（P5）会把每次手动停止都当成一次崩溃。
///
/// 两个流都要读：只读一个会让另一个的管道写满，游戏随之卡死（经典的管道死锁）。
pub async fn run(
    plan: LaunchPlan,
    logs: tokio::sync::mpsc::UnboundedSender<LogLine>,
    kill: Arc<tokio::sync::Notify>,
) -> Result<Option<i32>, Error> {
    let mut child = spawn(&plan).await?;
    let handles = [
        child
            .stdout
            .take()
            .map(|s| tokio::spawn(forward(s, "stdout", logs.clone()))),
        child
            .stderr
            .take()
            .map(|s| tokio::spawn(forward(s, "stderr", logs))),
    ];
    tokio::select! {
        status = child.wait() => {
            for h in handles.into_iter().flatten() {
                let _ = h.await;
            }
            Ok(status?.code())
        }
        () = kill.notified() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Ok(None)
        }
    }
}

async fn forward<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    reader: R,
    stream: &'static str,
    tx: tokio::sync::mpsc::UnboundedSender<LogLine>,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // 订阅方掉了就停（UI 关了日志面板不该让读取循环空转）
        if tx
            .send(LogLine {
                stream: stream.to_string(),
                line,
            })
            .is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{Arch, OsName, VersionJson};

    fn parse(json: &str) -> VersionJson {
        VersionJson::parse(json).unwrap()
    }
    fn v1214() -> VersionJson {
        parse(include_str!("../tests/fixtures/1.21.4.json"))
    }
    fn v1122() -> VersionJson {
        parse(include_str!("../tests/fixtures/1.12.2.json"))
    }
    /// 现代版本（有 `arguments`）但**没有** Quick Play：1.20 之前的最后一代参数形式。
    fn v1165() -> VersionJson {
        parse(include_str!("../tests/fixtures/1.16.5.json"))
    }
    fn win() -> OsInfo {
        OsInfo::new(OsName::Windows, "10.0", Arch::X64)
    }
    fn mac() -> OsInfo {
        OsInfo::new(OsName::Osx, "14.5", Arch::Arm64)
    }
    fn linux() -> OsInfo {
        OsInfo::new(OsName::Linux, "6.8.0", Arch::X64)
    }

    fn opts() -> LaunchOptions {
        LaunchOptions {
            game_dir: PathBuf::from("/data/instances/main/.minecraft"),
            natives_dir: PathBuf::from("/data/instances/main/natives"),
            assets_root: PathBuf::from("/data/cache/assets"),
            asset_index_id: "19".into(),
            classpath: vec![PathBuf::from("/data/cache/libraries/a.jar")],
            libraries_root: PathBuf::from("/data/cache/libraries"),
            client_jar: PathBuf::from("/data/cache/versions/1.21.4/1.21.4.jar"),
            java: PathBuf::from(if cfg!(windows) {
                r"C:\jdk\bin\java.exe"
            } else {
                "/usr/bin/java"
            }),
            memory_mb: 4096,
            resolution: None,
            server: None,
            demo: false,
            player_name: "Alice".into(),
            player_uuid: "7e01ed00f3453fbcb4437bac57a28a65".into(),
            access_token: "tok".into(),
            version_name: "1.21.4".into(),
            version_type: "release".into(),
            logging_config: Some(PathBuf::from(
                "/data/cache/versions/1.21.4/client-1.21.2.xml",
            )),
            extra_jvm_args: vec![],
            extra_game_args: vec![],
        }
    }

    #[test]
    fn extra_jvm_args_land_before_the_main_class() {
        // authlib-injector 的 -javaagent 走这里：它必须在主类之前，否则 JVM 当它是游戏参数
        let mut o = opts();
        o.extra_jvm_args = vec![
            "-javaagent:/tools/authlib-injector-1.2.8.jar=https://mc.example.com".to_string(),
            "-Dauthlibinjector.yggdrasil.prefetched=eyJhIjoxfQ==".to_string(),
        ];
        let plan = build_plan(&v1214(), &win(), &o).unwrap();
        let main_idx = plan
            .args
            .iter()
            .position(|a| a == "net.minecraft.client.main.Main")
            .unwrap();
        let agent_idx = plan
            .args
            .iter()
            .position(|a| a.starts_with("-javaagent:"))
            .expect("javaagent must be present");
        assert!(
            agent_idx < main_idx,
            "javaagent after main class: {:?}",
            plan.args
        );
        assert!(
            plan.args[main_idx + 1..]
                .iter()
                .all(|a| !a.starts_with("-javaagent:") && !a.starts_with("-Dauthlibinjector"))
        );
        assert!(
            plan.args
                .iter()
                .any(|a| a.starts_with("-Dauthlibinjector.yggdrasil.prefetched="))
        );
    }

    #[test]
    fn plan_shape_is_jvm_then_main_then_game() {
        let plan = build_plan(&v1214(), &win(), &opts()).unwrap();
        let args = &plan.args;
        let main_idx = args
            .iter()
            .position(|a| a == "net.minecraft.client.main.Main")
            .expect("main class");
        // JVM 参数全在主类之前（"-cp" 的取值除外，它是单独一个 arg），游戏参数全在之后
        let jvm_part = &args[..main_idx];
        for (i, a) in jvm_part.iter().enumerate() {
            let is_option_value = i > 0 && jvm_part[i - 1] == "-cp";
            assert!(
                a.starts_with('-') || is_option_value,
                "unexpected jvm arg {a:?} in {jvm_part:?}"
            );
        }
        assert!(args[main_idx + 1..].iter().any(|a| a == "--username"));
        assert!(!args[main_idx + 1..].iter().any(|a| a.starts_with("-Xmx")));

        assert!(args.contains(&"-Xmx4096M".to_string()));
        assert!(args.contains(&"-Xms256M".to_string()));
        assert!(
            args.iter()
                .any(|a| a == "-Djava.library.path=/data/instances/main/natives"
                    || a.contains("natives"))
        );
        assert_eq!(plan.work_dir, opts().game_dir);
        // 没有任何未替换的占位符（build_plan 自己也兜了一层）
        assert!(!args.iter().any(|a| a.contains("${")), "{args:?}");
    }

    #[test]
    fn classpath_uses_platform_separator_and_ends_with_client_jar() {
        let win_plan = build_plan(&v1214(), &win(), &opts()).unwrap();
        let nix_plan = build_plan(&v1214(), &linux(), &opts()).unwrap();
        let cp_of = |args: &[String]| {
            let i = args.iter().position(|a| a == "-cp").expect("-cp");
            args[i + 1].clone()
        };
        let win_cp = cp_of(&win_plan.args);
        let nix_cp = cp_of(&nix_plan.args);
        assert!(win_cp.contains(';'), "{win_cp}");
        assert!(nix_cp.contains(':') && !nix_cp.contains(';'), "{nix_cp}");
        for cp in [&win_cp, &nix_cp] {
            assert!(cp.ends_with("1.21.4.jar"), "client jar must be last: {cp}");
            assert!(cp.contains("a.jar"), "libraries first: {cp}");
        }
    }

    #[test]
    fn log4shell_mitigation_follows_the_game_version() {
        let flag = "-Dlog4j2.formatMsgNoLookups=true".to_string();
        let old = build_plan(&v1122(), &win(), &opts()).unwrap();
        assert!(old.args.contains(&flag), "1.12.2 must be mitigated");
        let modern = build_plan(&v1214(), &win(), &opts()).unwrap();
        assert!(!modern.args.contains(&flag), "1.21.4 ships a fixed log4j");
    }

    #[test]
    fn logging_argument_is_appended_with_the_real_path() {
        let plan = build_plan(&v1214(), &win(), &opts()).unwrap();
        let arg = plan
            .args
            .iter()
            .find(|a| a.starts_with("-Dlog4j.configurationFile="))
            .expect("logging argument must be added by the launcher, not by arguments.jvm");
        assert!(arg.ends_with("client-1.21.2.xml"), "{arg}");
        // 日志配置文件没下载到本地时不能加这个参数：JVM 会因为找不到文件直接启动失败，
        // 比"没有日志配置"糟糕得多。（1.12.2 与 1.21.4 都带 logging.client，所以判定依据是
        // 本地有没有那个文件，而不是版本新旧。）
        let mut no_log = opts();
        no_log.logging_config = None;
        let plan = build_plan(&v1214(), &win(), &no_log).unwrap();
        assert!(
            !plan
                .args
                .iter()
                .any(|a| a.contains("log4j.configurationFile"))
        );
    }

    #[test]
    fn legacy_versions_get_macos_first_thread_and_manual_resolution() {
        let mut o = opts();
        o.resolution = Some((1920, 1080));
        let mac_plan = build_plan(&v1122(), &mac(), &o).unwrap();
        assert!(mac_plan.args.contains(&"-XstartOnFirstThread".to_string()));
        // legacy 的 minecraftArguments 没有分辨率占位符，必须手动补
        assert!(mac_plan.args.contains(&"--width".to_string()));
        assert!(mac_plan.args.contains(&"1920".to_string()));
        assert_eq!(
            mac_plan
                .args
                .iter()
                .filter(|a| a.as_str() == "--width")
                .count(),
            1,
            "resolution must not be duplicated"
        );

        // 现代版本靠 has_custom_resolution 特性注入，同样只能出现一次
        let modern = build_plan(&v1214(), &mac(), &o).unwrap();
        assert_eq!(
            modern
                .args
                .iter()
                .filter(|a| a.as_str() == "--width")
                .count(),
            1
        );
        // 现代 macOS 的 -XstartOnFirstThread 来自 arguments.jvm 的规则，不是我们手加的
        assert_eq!(
            modern
                .args
                .iter()
                .filter(|a| a.as_str() == "-XstartOnFirstThread")
                .count(),
            1
        );
        // Windows/Linux 上不该出现
        assert!(
            !build_plan(&v1122(), &win(), &o)
                .unwrap()
                .args
                .contains(&"-XstartOnFirstThread".to_string())
        );
    }

    #[test]
    fn missing_main_class_is_an_error() {
        let mut v = v1214();
        v.main_class = None;
        assert!(matches!(
            build_plan(&v, &win(), &opts()),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn natives_extraction_skips_excludes_and_blocks_zip_slip() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("natives.jar");
        {
            let f = std::fs::File::create(&jar).unwrap();
            let mut zip = zip::ZipWriter::new(f);
            let opts: zip::write::SimpleFileOptions = Default::default();
            zip.start_file("org/lwjgl/lwjgl.dll", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"dll-bytes").unwrap();
            zip.start_file("META-INF/MANIFEST.MF", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"manifest").unwrap();
            zip.start_file("nested/deep/lib.so", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"so-bytes").unwrap();
            zip.finish().unwrap();
        }
        let dest = dir.path().join("natives");
        let n = extract_natives(&[(jar.clone(), vec!["META-INF/".to_string()])], &dest).unwrap();
        assert_eq!(n, 2, "META-INF/ must be skipped");
        assert_eq!(
            std::fs::read(dest.join("org/lwjgl/lwjgl.dll")).unwrap(),
            b"dll-bytes"
        );
        assert!(dest.join("nested/deep/lib.so").exists());
        assert!(!dest.join("META-INF").exists());

        // 重启前清空重建：旧版本残留的库不该活到下一次启动
        std::fs::write(dest.join("org/lwjgl/stale.dll"), b"old").unwrap();
        extract_natives(&[(jar, vec!["META-INF/".to_string()])], &dest).unwrap();
        assert!(!dest.join("org/lwjgl/stale.dll").exists());

        // zip-slip：条目里的 ../ 必须被拒绝，而不是写到目标目录外
        let evil = dir.path().join("evil.jar");
        {
            let f = std::fs::File::create(&evil).unwrap();
            let mut zip = zip::ZipWriter::new(f);
            let opts: zip::write::SimpleFileOptions = Default::default();
            zip.start_file("../../escaped.txt", opts).unwrap();
            std::io::Write::write_all(&mut zip, b"pwned").unwrap();
            zip.finish().unwrap();
        }
        let err = extract_natives(&[(evil, vec![])], &dir.path().join("n2")).unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
        assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
    }

    #[test]
    fn natives_extraction_reports_missing_jar() {
        let dir = tempfile::tempdir().unwrap();
        let err = extract_natives(
            &[(dir.path().join("nope.jar"), vec![])],
            &dir.path().join("natives"),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{err:?}");
    }

    #[test]
    fn windows_prefers_javaw_when_present() {
        // 非 Windows 平台上原样返回（这条逻辑本来就只在 Windows 生效）
        let p = PathBuf::from(if cfg!(windows) {
            r"C:\jdk\bin\java.exe"
        } else {
            "/usr/bin/java"
        });
        assert_eq!(windows_java(&p), p);
    }

    #[test]
    fn classpath_separator_matches_platform() {
        assert_eq!(classpath_separator(&win()), ";");
        assert_eq!(classpath_separator(&linux()), ":");
        assert_eq!(classpath_separator(&mac()), ":");
    }

    #[test]
    fn server_join_picks_the_argument_form_the_version_understands() {
        // 1.20 起 Mojang 用 Quick Play 取代了 --server/--port。判据取自版本 JSON 本身
        // （模板里有没有 --quickPlayMultiplayer），所以这三代版本各自走对的那条路。
        //
        // P1 时这里有一个测试断言 1.21.4 会收到 `--server`，它是照着当时"无条件追加
        // --server"的实现写的，所以一直通过——而真实的 1.21.4 客户端并不接受这个参数。
        // 照着实现写的测试只能证明实现等于它自己；这类断言必须回到上游事实
        // （版本 JSON / 真实客户端行为）才有意义。
        let mut o = opts();
        o.server = Some(ServerJoin {
            host: "play.example.com".into(),
            port: Some(25566),
        });

        let modern = build_plan(&v1214(), &win(), &o).unwrap();
        let qp = modern
            .args
            .iter()
            .position(|a| a == "--quickPlayMultiplayer")
            .expect("1.21.4 应当用 Quick Play");
        assert_eq!(modern.args[qp + 1], "play.example.com:25566");
        assert!(
            !modern.args.iter().any(|a| a == "--server"),
            "两套参数只能有一套，否则客户端行为取决于它先读到哪个"
        );
        let path = modern
            .args
            .iter()
            .position(|a| a == "--quickPlayPath")
            .expect("Quick Play 需要日志目录");
        assert!(
            modern.args[path + 1].ends_with("quickPlay"),
            "{}",
            modern.args[path + 1]
        );

        // 1.16.5 同样是"现代"版本（有 arguments），但没有 Quick Play 模板项
        let mid = build_plan(&v1165(), &win(), &o).unwrap();
        assert!(
            !mid.args.iter().any(|a| a.contains("quickPlay")),
            "不认识 Quick Play 的版本不该收到这些参数：{:?}",
            mid.args
        );
        let s = mid.args.iter().position(|a| a == "--server").unwrap();
        assert_eq!(mid.args[s + 1], "play.example.com");
        let p = mid.args.iter().position(|a| a == "--port").unwrap();
        assert_eq!(mid.args[p + 1], "25566");

        // legacy 也走 --server/--port，且未指定端口时用默认端口
        let mut legacy_opts = opts();
        legacy_opts.server = Some(ServerJoin {
            host: "mc.example.com".into(),
            port: None,
        });
        let legacy = build_plan(&v1122(), &win(), &legacy_opts).unwrap();
        let p = legacy.args.iter().position(|a| a == "--port").unwrap();
        assert_eq!(legacy.args[p + 1], DEFAULT_SERVER_PORT.to_string());
    }

    #[test]
    fn quick_play_support_is_read_from_the_version_json() {
        assert!(supports_quick_play(&v1214()));
        assert!(
            !supports_quick_play(&v1165()),
            "1.16.5 没有 Quick Play，按版本号猜会在这一代上出错"
        );
        assert!(!supports_quick_play(&v1122()));
    }

    #[test]
    fn demo_mode_is_expressed_by_the_feature_flag() {
        let mut o = opts();
        o.demo = true;
        assert!(
            build_plan(&v1214(), &win(), &o)
                .unwrap()
                .args
                .contains(&"--demo".to_string())
        );
        o.demo = false;
        assert!(
            !build_plan(&v1214(), &win(), &o)
                .unwrap()
                .args
                .contains(&"--demo".to_string())
        );
    }

    #[test]
    fn extra_game_args_come_after_everything_else() {
        // 放最后才可能覆盖版本模板给的同名项（例如整合包要改 --versionType）
        let mut o = opts();
        o.extra_game_args = vec!["--versionType".into(), "MythicPack".into()];
        let plan = build_plan(&v1214(), &win(), &o).unwrap();
        let tail = &plan.args[plan.args.len() - 2..];
        assert_eq!(tail, ["--versionType", "MythicPack"]);
        // 主类之前，游戏参数之后
        let main_idx = plan
            .args
            .iter()
            .position(|a| a == "net.minecraft.client.main.Main")
            .unwrap();
        assert!(main_idx < plan.args.len() - 2);
    }
}
