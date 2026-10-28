//! NeoForge 真装（默认忽略：要联网、要下 ~700MB、要跑 6 个 Java 子进程）。
//!
//! 这是 W9 唯一的真验收：processor pipeline 的每一步都只有真跑一遍才知道对不对
//! （坐标拼错、变量少认一个、主类读不到、sides 过滤错，症状全都是"Java 那边报一句
//! 看不出根因的错"）。单测能钉住解析与规划，钉不住"跑出来的产物是不是对的"。
//!
//! 用法：
//! ```text
//! cargo test --release --test neoforge_live -- --ignored --nocapture
//! # 可选：COMPASS_NEOFORGE_DIR=<数据目录>（复用已下载的库，省掉重装原版的时间）
//! #       COMPASS_NEOFORGE_VERSION=21.4.161（默认取该游戏版本下最新的一个）
//! #       COMPASS_NEOFORGE_GAME=1.21.4
//! ```

use std::path::PathBuf;

use compass_lib::api::ApiClient;
use compass_lib::install::Installer;
use compass_lib::loader::{self, LoaderKind};
use compass_lib::meta::MetaClient;
use compass_lib::net::Downloader;
use compass_lib::paths::Paths;
use compass_lib::version::OsInfo;

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

#[tokio::test]
#[ignore = "needs network, ~700MB of downloads and a Java runtime"]
async fn installs_neoforge_for_real() {
    let game = env_or("COMPASS_NEOFORGE_GAME", "1.21.4");
    // 数据目录默认用临时目录；给 COMPASS_NEOFORGE_DIR 时可以复用上一次下载的东西
    let holder;
    let data = match std::env::var("COMPASS_NEOFORGE_DIR").ok() {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => {
            holder = tempfile::tempdir().unwrap();
            holder.path().join("data")
        }
    };
    let paths = Paths::new(data);
    paths.ensure().unwrap();
    let http = reqwest::Client::new();
    let api = ApiClient::new(http.clone());
    let installer = Installer::new(
        MetaClient::new(http.clone(), String::new()),
        Downloader::new(http.clone(), 8, String::new()),
        paths.clone(),
    );

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
    let chosen = env_or("COMPASS_NEOFORGE_VERSION", &versions[0].version);
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
    let produced = walkdir_names(
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
    let extracted = walkdir_names(&paths.cache().join("forge-extract"));
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
    };
    let plan = compass_lib::runner::build(&compass_lib::runner::LaunchInput {
        instance: &inst,
        account: &account,
        version: &merged,
        os: &OsInfo::current(),
        paths: &paths,
        default_memory_mb: 4096,
        api_url: "https://mc.example.com",
        java: java_path(),
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

/// 列出一个目录树下所有文件名（不引 walkdir 依赖，测试里够用就行）。
fn walkdir_names(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else if let Some(n) = e.file_name().to_str() {
                out.push(n.to_string());
            }
        }
    }
    out.sort();
    out
}

fn java_path() -> PathBuf {
    if let Ok(home) = std::env::var("JAVA_HOME")
        && !home.is_empty()
    {
        let p =
            PathBuf::from(&home)
                .join("bin")
                .join(if cfg!(windows) { "java.exe" } else { "java" });
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(if cfg!(windows) { "java.exe" } else { "java" })
}
