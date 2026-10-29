//! NeoForge 真装与真启动（默认忽略：要联网、要下 ~700MB、要跑 6 个 Java 子进程）。
//!
//! 这是 W9 的真验收：processor pipeline 的每一步都只有真跑一遍才知道对不对
//! （坐标拼错、变量少认一个、主类读不到、sides 过滤错，症状全都是"Java 那边报一句
//! 看不出根因的错"）。单测能钉住解析与规划，钉不住"跑出来的产物是不是对的"。
//!
//! 两个测试，CI 只跑第一个：
//! - `installs_neoforge_for_real`：装 + 产物落盘 + 启动计划自洽（三平台都跑）。
//! - `launches_neoforge_for_real`：真启动到客户端引导（要显示服务，本地跑）。
//!
//! 用法：
//! ```text
//! cargo test --test neoforge_live -- --ignored --nocapture
//! # 可选：COMPASS_NEOFORGE_DIR=<数据目录>（复用已下载的库，省掉重装原版的时间）
//! #       COMPASS_NEOFORGE_VERSION=21.4.161（默认取该游戏版本下最新的一个）
//! #       COMPASS_NEOFORGE_GAME=1.21.4
//! ```
//! 别加 `--release`：`[profile.release] panic = "abort"` 与测试要的 unwind 冲突。

mod live;

use std::path::PathBuf;

use compass_lib::loader::{self, LoaderKind};
use compass_lib::paths::Paths;
use compass_lib::version::OsInfo;

#[tokio::test]
#[ignore = "needs network, ~700MB of downloads and a Java runtime"]
async fn installs_neoforge_for_real() {
    let game = live::env_or("COMPASS_NEOFORGE_GAME", "1.21.4");
    // 数据目录默认用临时目录；给 COMPASS_NEOFORGE_DIR 时可以复用上一次下载的东西
    let data = live::data_dir("COMPASS_NEOFORGE_DIR");
    let paths = Paths::new(data.path().to_path_buf());
    paths.ensure().unwrap();
    let http = live::http();
    let api = live::api(&http);
    let installer = live::installer(&paths, &http);

    // 1. 版本列表：Maven 元数据 + 按游戏版本过滤
    let versions = loader::versions(&api, LoaderKind::Neoforge, &game)
        .await
        .unwrap();
    assert!(!versions.is_empty(), "没列出任何 NeoForge 版本");
    let expected_prefix = game.strip_prefix("1.").map(|g| format!("{g}.")).unwrap();
    assert!(
        versions
            .iter()
            .all(|v| v.version.starts_with(&expected_prefix)),
        "列表里混进了别的游戏版本的 NeoForge：{:?}",
        versions
            .iter()
            .take(5)
            .map(|v| &v.version)
            .collect::<Vec<_>>()
    );
    // Maven 元数据没有 stable 的概念，必须是 None（不能显示成"开发版"）
    assert!(
        versions.iter().all(|v| v.stable.is_none()),
        "NeoForge 的 stable 应当全是 None"
    );
    let chosen = live::env_or("COMPASS_NEOFORGE_VERSION", &versions[0].version);
    println!("neoforge versions: {} (chosen {chosen})", versions.len());

    // 2. 装（父版本 → installer → processors → 子版本，全在 loader::install 里）
    let installed = loader::install(&installer, &api, LoaderKind::Neoforge, &game, &chosen, None)
        .await
        .unwrap();
    println!(
        "installed: id={} jar={} main={} java={} downloaded={} skipped={}",
        installed.id,
        installed.jar_version,
        installed.main_class,
        installed.java_major,
        installed.download.downloaded,
        installed.download.skipped
    );
    assert_eq!(installed.id, format!("neoforge-{chosen}"));
    assert_eq!(
        installed.jar_version, game,
        "client jar 来自父版本（继承型版本自己不带 jar）"
    );
    assert_eq!(
        installed.main_class,
        "cpw.mods.bootstraplauncher.BootstrapLauncher"
    );
    assert_eq!(installed.download.failed, Vec::<String>::new());

    // 3. processor 的产物必须真的在磁盘上（这是 pipeline 跑通的证据）
    let fssep = std::path::MAIN_SEPARATOR_STR;
    let lib = |rel: &str| paths.libraries().join(rel.replace('/', fssep));
    let patched = lib(&format!(
        "net/neoforged/neoforge/{chosen}/neoforge-{chosen}-client.jar"
    ));
    assert!(
        patched.is_file(),
        "binarypatcher 的产物不存在：{}",
        patched.display()
    );
    let patched_len = std::fs::metadata(&patched).unwrap().len();
    assert!(
        patched_len > 1_000_000,
        "补丁 jar 只有 {patched_len} 字节，不像是打过补丁的完整客户端"
    );
    let universal = lib(&format!(
        "net/neoforged/neoforge/{chosen}/neoforge-{chosen}-universal.jar"
    ));
    assert!(
        universal.is_file(),
        "universal jar 不在：{}",
        universal.display()
    );

    // jarsplitter 与 AutoRenamingTool 的中间产物：名字里带 neoform 时间戳，
    // 所以按关键字认，不写死文件名
    let produced = live::walkdir_names(
        &paths
            .libraries()
            .join("net")
            .join("minecraft")
            .join("client"),
    );
    println!("minecraft client artifacts: {produced:?}");
    for expect in ["slim", "srg", "extra"] {
        assert!(
            produced.iter().any(|n| n.contains(expect)),
            "缺少 {expect} 产物：{produced:?}"
        );
    }
    // BINPATCH（installer jar 里的 data/client.lzma）必须被解出来过：
    // 它不在 libraries 下，而在 cache 的 forge-extract 里
    let extracted = live::walkdir_names(&paths.cache().join("forge-extract"));
    println!("extracted installer entries: {extracted:?}");
    assert!(
        extracted.iter().any(|n| n.ends_with(".lzma")),
        "client.lzma 没被解出来：{extracted:?}"
    );

    // 4. 合并后的版本 JSON 落盘（离线可启动），且 classpath 里不放任何游戏 jar
    let json = compass_lib::runner::installed_version_path(&paths, &installed.id);
    assert!(json.is_file(), "{} 没落盘", json.display());
    let merged = compass_lib::runner::read_installed_version(&paths, &installed.id).unwrap();
    assert_eq!(merged.inherits_from.as_deref(), Some(game.as_str()));
    assert!(compass_lib::version::loader_provides_game_jar(&merged));

    let inst = compass_lib::instance::Instance {
        slug: "nf-live".into(),
        name: "NeoForge live".into(),
        version_id: installed.id.clone(),
        ..Default::default()
    };
    compass_lib::instance::save(&paths, &inst).unwrap();
    let account = compass_lib::account::Account {
        player_name: "Tester".into(),
        player_uuid: "069a79f444e94726a5befca90e38aaf5".into(),
        access_token: "0".repeat(32),
        client_token: "0".repeat(32),
        api_url: "https://mc.example.com".into(),
        signed_in_at: 0,
        kind: compass_lib::account::AccountKind::Yggdrasil,
    };
    let plan = compass_lib::runner::build(&compass_lib::runner::LaunchInput {
        instance: &inst,
        account: &account,
        version: &merged,
        os: &OsInfo::current(),
        paths: &paths,
        default_memory_mb: 4096,
        api_url: "https://mc.example.com",
        java: live::java_path(),
        authlib_jar: None,
        prefetched_meta: None,
    })
    .unwrap();

    let cp_idx = plan.args.iter().position(|a| a == "-cp").unwrap();
    let cp = &plan.args[cp_idx + 1];
    let sep = if cfg!(windows) { ';' } else { ':' };
    let entries: Vec<&str> = cp.split(sep).collect();
    println!("classpath entries: {}", entries.len());
    // 一个游戏 jar 都不能有：原版 jar 与补丁 jar 都会撞自动模块名（实测报错见
    // version::loader_provides_game_jar 的注释）
    for e in &entries {
        assert!(
            !e.ends_with(&format!("{game}.jar")),
            "原版 jar 不该在 classpath 上：{e}"
        );
        assert!(
            !e.contains("neoforge-") || !e.contains("-client.jar"),
            "补丁 jar 不该在 classpath 上：{e}"
        );
    }
    // 每一项都必须在磁盘上（少一个 jar，JVM 抛的是 ClassNotFoundException）
    for e in &entries {
        assert!(PathBuf::from(e).is_file(), "classpath entry missing: {e}");
    }
    // asm 只留子版本那份（父版本 9.6 与子版本 9.8 同时在的话 BootstrapLauncher 会拒）
    let asm: Vec<&&str> = entries
        .iter()
        .filter(|e| e.contains("org/ow2/asm/asm/") || e.contains("org\\ow2\\asm\\asm\\"))
        .collect();
    assert_eq!(asm.len(), 1, "asm 应当只剩一份：{asm:?}");

    // ModLauncher 的入口参数与模块路径都要在
    let lt = plan
        .args
        .iter()
        .position(|a| a == "--launchTarget")
        .expect("--launchTarget 必须在");
    assert_eq!(plan.args[lt + 1], "neoforgeclient");
    assert!(plan.args.iter().any(|a| a == "-p"), "模块路径参数丢了");
    assert!(plan.args.iter().any(|a| a == "--add-modules"));
    assert!(
        plan.args
            .iter()
            .any(|a| a == "cpw.mods.bootstraplauncher.BootstrapLauncher")
    );
    assert!(
        !plan.args.iter().any(|a| a.contains("${")),
        "残留占位符：{:?}",
        plan.args
    );
    println!(
        "neoforge live install + launch plan ok ({} cp entries)",
        entries.len()
    );
}

/// 把一条流逐行收进共享日志。两个流之间没有全局顺序，日志只用于找标记与看末尾。
async fn pump_lines<R>(stream: R, sink: std::sync::Arc<std::sync::Mutex<Vec<String>>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        sink.lock().unwrap().push(line);
    }
}

/// 真启动**我们自己装出来的**那份 NeoForge——W9 验收里"装完真启动"这半。
///
/// 上面那个测试钉的是启动计划（classpath 组成、模块参数、asm 只有一份），
/// 计划对不等于跑得起来：模块名撞车、fml_loader 找不到补丁 jar、少一个依赖，
/// 全都只有 JVM 真启动才暴露。
///
/// 不能等进程退出：这台机器可能有显示服务，游戏会真开窗一直跑。
/// 所以边收集输出边等标记，见到就杀进程。`Backend library: LWJGL` 是它需要窗口
/// 之前的最后一步——走到这里就说明 classpath、自动模块名、fml_loader 按坐标找
/// 补丁 jar、Mixin、Datafixer 全对了。
#[tokio::test]
#[ignore = "needs network, ~700MB of downloads, Java 21 and a display (GLFW opens a window)"]
async fn launches_neoforge_for_real() {
    const REACHED: &str = "Backend library: LWJGL";
    // 这几条都是"classpath 组装错了"的特征症状，出现即失败（原因见
    // version::loader_provides_game_jar 的注释：两种 jar 都会撞自动模块名）
    const FATAL: [&str; 4] = [
        "Error occurred during initialization",
        "export package net.minecraft",
        "reads more than one module named",
        "ClassNotFoundException",
    ];

    let game = live::env_or("COMPASS_NEOFORGE_GAME", "1.21.4");
    let data = live::data_dir("COMPASS_NEOFORGE_DIR");
    let paths = Paths::new(data.path().to_path_buf());
    paths.ensure().unwrap();
    let http = live::http();
    let api = live::api(&http);
    let installer = live::installer(&paths, &http);
    let versions = loader::versions(&api, LoaderKind::Neoforge, &game)
        .await
        .unwrap();
    let chosen = live::env_or("COMPASS_NEOFORGE_VERSION", &versions[0].version);
    let installed = loader::install(&installer, &api, LoaderKind::Neoforge, &game, &chosen, None)
        .await
        .unwrap();
    println!(
        "installed {} ({} files)",
        installed.id, installed.download.downloaded
    );

    let inst = compass_lib::instance::Instance {
        slug: "nf-launch".into(),
        name: "NeoForge launch".into(),
        version_id: installed.id.clone(),
        ..Default::default()
    };
    compass_lib::instance::save(&paths, &inst).unwrap();
    let account = compass_lib::account::Account {
        player_name: "Tester".into(),
        player_uuid: "069a79f444e94726a5befca90e38aaf5".into(),
        access_token: "0".repeat(32),
        client_token: "0".repeat(32),
        api_url: "https://mc.example.com".into(),
        signed_in_at: 0,
        kind: compass_lib::account::AccountKind::Yggdrasil,
    };
    let version = compass_lib::runner::read_installed_version(&paths, &installed.id).unwrap();
    let os = OsInfo::current();
    let input = compass_lib::runner::LaunchInput {
        instance: &inst,
        account: &account,
        version: &version,
        os: &os,
        paths: &paths,
        default_memory_mb: 4096,
        api_url: "https://mc.example.com",
        java: live::java_path(),
        authlib_jar: None,
        prefetched_meta: None,
    };
    let plan = compass_lib::runner::build(&input).unwrap();
    let natives = compass_lib::runner::natives_to_extract(&input).unwrap();
    let natives_dir = compass_lib::instance::natives_dir(&paths, &inst.slug).unwrap();
    println!(
        "natives extracted: {}",
        compass_lib::launch::extract_natives(&natives, &natives_dir).unwrap()
    );

    let mut child = compass_lib::launch::spawn(&plan).await.unwrap();
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    // stdout 与 stderr 是两个不同的类型，只能各自 spawn（合并到一个数组里编不过）
    if let Some(stream) = child.stdout.take() {
        let sink = std::sync::Arc::clone(&log);
        tokio::spawn(pump_lines(stream, sink));
    }
    if let Some(stream) = child.stderr.take() {
        let sink = std::sync::Arc::clone(&log);
        tokio::spawn(pump_lines(stream, sink));
    }

    let started = std::time::Instant::now();
    let limit = std::time::Duration::from_secs(420);
    let mut reached: Option<String> = None;
    let mut fatal: Option<String> = None;
    let mut exited: Option<Option<i32>> = None;
    while started.elapsed() < limit {
        {
            let lines = log.lock().unwrap();
            reached = lines.iter().find(|l| l.contains(REACHED)).cloned();
            fatal = lines
                .iter()
                .find(|l| FATAL.iter().any(|f| l.contains(f)))
                .cloned();
        }
        if reached.is_some() || fatal.is_some() {
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                exited = Some(status.code());
                break;
            }
            Ok(None) => {}
            Err(e) => panic!("wait failed: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let _ = child.kill().await;

    let lines = log.lock().unwrap().clone();
    let log_path = data.path().join("launch.log");
    std::fs::write(&log_path, lines.join("\n")).ok();
    println!(
        "{} lines in {:.0}s, exit={:?}, log={}",
        lines.len(),
        started.elapsed().as_secs_f64(),
        exited,
        log_path.display()
    );
    let tail = |n: usize| -> String {
        let skip = lines.len().saturating_sub(n);
        lines[skip..].join("\n")
    };

    // 走到需要窗口之前的最后一步。标记只挑结构稳定的三行：
    // ModLauncher 的 target、原版认下账号与目录、LWJGL 绑定完成。
    // （别照着某一次的日志挑中间那些：Datafixer 那行的 logger 名在不同版本里
    // 是 c.g.m.ModdedDatafixers 或 mojang/DataFixerBuilder，写死就是自找的假红。）
    for marker in [
        "Launching target 'neoforgeclient'",
        "Setting user:",
        REACHED,
    ] {
        assert!(
            lines.iter().any(|l| l.contains(marker)),
            "没走到 {marker:?}（exit={exited:?}）；末尾：\n{}",
            tail(30)
        );
    }
    assert!(
        fatal.is_none(),
        "启动失败于 {:?}；末尾：\n{}",
        fatal,
        tail(30)
    );
    println!(
        "neoforge real launch reached the client bootstrap: {:?}",
        reached
    );
}
