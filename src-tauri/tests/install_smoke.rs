//! 真下载冒烟测试（P1 的验收手段之一）。
//!
//! 默认 `#[ignore]`：要联网、要下约 550MB、要几分钟。手动跑：
//! `cargo test --test install_smoke -- --ignored --nocapture`
//! CI 里作为 P1 的验收步骤在三平台各跑一次（计划文档"前置决策 6"）。
//!
//! 它验的是单元测试覆盖不到的那一段：**真实网络上装出来的东西，与我们拼出来的命令行
//! 是否自洽**——classpath 上每一个条目都真的在磁盘上、资源对象数量与索引一致、
//! 日志配置文件确实在我们以为的位置。
//!
//! 设 `COMPASS_SMOKE_LAUNCH=1` 时会真的拉起游戏，等日志出现主菜单标志后杀掉进程
//! （需要可用的显示环境；会短暂弹出游戏窗口）。

use std::path::PathBuf;

use compass_lib::assets::{self, AssetIndex};
use compass_lib::install::{Installer, split_libraries};
use compass_lib::launch::{self, LaunchOptions};
use compass_lib::meta::MetaClient;
use compass_lib::net::Downloader;
use compass_lib::paths::Paths;
use compass_lib::version::{OsInfo, VersionJson};

const VERSION: &str = "1.21.4";

#[tokio::test]
#[ignore = "needs network and ~550MB of downloads"]
async fn installs_a_real_version_and_produces_a_consistent_launch_plan() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join("data"));
    paths.ensure().unwrap();

    let http = reqwest::Client::new();
    // 官方资源 CDN（resources.download.minecraft.net）在部分网络下整体不可达；
    // 设 COMPASS_SMOKE_MIRROR 可走 BMCLAPI 兼容镜像（顺带把镜像重写这条路径也验了）。
    let mirror = std::env::var("COMPASS_SMOKE_MIRROR").unwrap_or_default();
    let installer = Installer::new(
        MetaClient::new(http.clone(), mirror.clone()),
        Downloader::new(http.clone(), 16, mirror),
        paths.clone(),
    );

    // 1. 真装一遍
    let installed = installer.install(VERSION, None).await.unwrap();
    assert_eq!(installed.main_class, "net.minecraft.client.main.Main");
    assert_eq!(installed.java_major, 21);
    assert_eq!(installed.download.failed, Vec::<String>::new());
    assert!(
        installed.download.downloaded > 100,
        "expected lots of files, got {:?}",
        installed.download
    );

    // 2. 幂等：再装一次应该全是 skipped，且不报失败
    let again = installer.install(VERSION, None).await.unwrap();
    assert_eq!(again.download.failed, Vec::<String>::new());
    assert_eq!(again.download.downloaded, 0, "{:?}", again.download);
    assert!(again.download.skipped > 100);

    // 3. 落盘检查
    let jar = installer.client_jar_path(VERSION);
    assert!(jar.is_file(), "client jar missing: {}", jar.display());
    assert!(std::fs::metadata(&jar).unwrap().len() > 20_000_000);
    let version_json = installer.version_json_path(VERSION);
    assert!(
        version_json.is_file(),
        "merged version json should be on disk (离线启动靠它)"
    );
    let merged: VersionJson =
        serde_json::from_str(&std::fs::read_to_string(&version_json).unwrap()).unwrap();
    assert_eq!(
        merged.main_class.as_deref(),
        Some("net.minecraft.client.main.Main")
    );

    // 资源：索引在磁盘上，且**每个** object 都真的落盘了（内容寻址，一个都不能少）
    let index_path = paths
        .assets()
        .join(assets::index_rel_path(&installed.asset_index_id));
    let index = AssetIndex::parse(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
    let tasks = assets::download_tasks(&index, &paths.assets());
    assert!(tasks.len() > 1000, "expected thousands of asset objects");
    let missing: Vec<_> = tasks.iter().filter(|t| !t.dest.is_file()).collect();
    assert!(
        missing.is_empty(),
        "{} asset objects missing",
        missing.len()
    );
    // 1.21.4 是现代版本，不需要 virtual 布局
    assert!(!assets::needs_virtual_layout(VERSION));

    // 4. 命令行自洽：classpath 上每一个条目都必须真的存在
    let os = OsInfo::current();
    let (classpath, natives) = split_libraries(&merged, &os, &paths);
    assert!(!classpath.is_empty());
    let missing_libs: Vec<_> = classpath.iter().filter(|p| !p.is_file()).collect();
    assert!(
        missing_libs.is_empty(),
        "{} classpath entries missing, e.g. {:?}",
        missing_libs.len(),
        missing_libs.first().map(|p| p.display().to_string())
    );
    // 1.21.4 的原生库走 classpath（LWJGL3），没有要解压的老式 natives
    assert!(
        natives.is_empty(),
        "modern version should have no extract natives"
    );

    // 5. 启动计划：无残留占位符，classpath 与磁盘一致
    let natives_dir = dir.path().join("natives");
    let game_dir = dir.path().join("game");
    std::fs::create_dir_all(&game_dir).unwrap();
    let opts = LaunchOptions {
        game_dir: game_dir.clone(),
        natives_dir: natives_dir.clone(),
        assets_root: paths.assets(),
        asset_index_id: installed.asset_index_id.clone(),
        classpath,
        libraries_root: paths.libraries(),
        client_jar: jar.clone(),
        java: java_path(),
        memory_mb: 2048,
        resolution: None,
        server: None,
        demo: false,
        player_name: "SmokeTester".into(),
        // 冒烟测试不连后端：离线模式下客户端只是把这串 UUID 当身份用，不校验
        player_uuid: "069a79f444e94726a5befca90e38aaf5".into(),
        access_token: "0".repeat(32),
        version_name: VERSION.into(),
        version_type: "release".into(),
        logging_config: logging_file(&installer, VERSION, &merged),
        extra_jvm_args: vec![],
    };
    let plan = launch::build_plan(&merged, &os, &opts).unwrap();
    assert!(
        !plan.args.iter().any(|a| a.contains("${")),
        "unsubstituted placeholder: {:?}",
        plan.args
    );
    assert!(plan.args.iter().any(|a| a == "-Xmx2048M"));
    let cp_idx = plan.args.iter().position(|a| a == "-cp").unwrap();
    let sep = if cfg!(windows) { ';' } else { ':' };
    let cp_entries: Vec<_> = plan.args[cp_idx + 1].split(sep).collect();
    // 各平台通过规则的库条数不同（Windows 84 / macOS 77 / Linux 70），故只给下界
    assert!(
        cp_entries.len() > 60,
        "classpath looks too short: {}",
        cp_entries.len()
    );
    assert_eq!(PathBuf::from(cp_entries.last().unwrap()), jar);
    for entry in &cp_entries {
        assert!(
            PathBuf::from(entry).is_file(),
            "classpath entry missing: {entry}"
        );
    }

    // 6. 可选：真的把游戏拉起来。走 runner::prepare_launch——与 instance_launch 命令
    //    是同一条代码路径，所以这里验的就是用户会跑到的东西（而不是测试自己重写一遍顺序）。
    if std::env::var("COMPASS_SMOKE_LAUNCH").is_err() {
        return;
    }

    let api_url =
        std::env::var("COMPASS_SMOKE_API").unwrap_or_else(|_| "http://localhost:8080".into());
    let cfg = compass_lib::config::Config {
        api_url: api_url.clone(),
        defaults: compass_lib::config::Defaults { memory_mb: 2048 },
        ..Default::default()
    };

    // 给了 COMPASS_SMOKE_PLAYER/PASSWORD 就用真账号登录（顺带验 ADR-0012 的启动器登录链路：
    // 玩家名 + 该玩家的独立密码），否则用假身份（客户端启动时不校验令牌，只够验到主菜单）
    let account = match (
        std::env::var("COMPASS_SMOKE_PLAYER").ok(),
        std::env::var("COMPASS_SMOKE_PASSWORD").ok(),
    ) {
        (Some(player), Some(password)) => {
            let api = compass_lib::api::ApiClient::new(http.clone());
            let resp = api
                .authenticate(&cfg.api_url, &player, &password)
                .await
                .unwrap();
            let profile = resp.selected_profile.unwrap();
            println!("signed in as {} ({})", profile.name, profile.id);
            compass_lib::account::Account {
                player_name: profile.name,
                player_uuid: profile.id,
                access_token: resp.access_token,
                client_token: resp.client_token,
                api_url: cfg.api_url.clone(),
                signed_in_at: 0,
            }
        }
        _ => compass_lib::account::Account {
            player_name: "SmokeTester".into(),
            player_uuid: "069a79f444e94726a5befca90e38aaf5".into(),
            access_token: "0".repeat(32),
            client_token: "0".repeat(32),
            api_url: cfg.api_url.clone(),
            signed_in_at: 0,
        },
    };

    let inst = compass_lib::instance::Instance {
        slug: "smoke".into(),
        name: "Smoke".into(),
        version_id: VERSION.into(),
        memory_mb: Some(2048),
        created_at: 0,
        ..compass_lib::instance::Instance::default()
    };
    compass_lib::instance::save(&paths, &inst).unwrap();

    let prepared = compass_lib::runner::prepare_launch(&paths, &http, &cfg, &inst, &account)
        .await
        .unwrap();
    assert!(
        prepared.injected,
        "authlib-injector 没注入：皮肤站会认不到（命令行里没有 -javaagent）"
    );
    assert!(
        prepared.plan.args.iter().any(|a| a.contains(&api_url)),
        "injector 应指向我们的皮肤站 {api_url}"
    );
    println!("java {} {:?}", prepared.java_version, prepared.plan.program);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let kill = std::sync::Arc::new(tokio::sync::Notify::new());
    let supervisor = tokio::spawn(launch::run(prepared.plan, tx, std::sync::Arc::clone(&kill)));

    let mut seen_user = false;
    let mut seen_lwjgl = false;
    let mut seen_injector = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    while tokio::time::Instant::now() < deadline && !(seen_user && seen_lwjgl && seen_injector) {
        let Ok(Some(line)) =
            tokio::time::timeout(std::time::Duration::from_secs(90), rx.recv()).await
        else {
            break;
        };
        println!("[{}] {}", line.stream, line.line);
        if line.line.contains("Setting user:") || line.line.contains("(Session ID is") {
            seen_user = true;
        }
        if line.line.contains("Backend library: LWJGL") {
            seen_lwjgl = true;
        }
        if line.line.contains("[authlib-injector]") {
            seen_injector = true;
        }
    }

    // 主动停止：退出码 None 表示"被我们杀掉"，与崩溃必须能区分
    kill.notify_one();
    let exited = tokio::time::timeout(std::time::Duration::from_secs(60), supervisor)
        .await
        .expect("游戏应当在停止信号后退出")
        .unwrap()
        .unwrap();
    assert_eq!(exited, None, "被停止的进程不该有退出码");
    assert!(seen_injector, "日志里没有 [authlib-injector]：注入没生效");
    assert!(seen_user, "游戏没到 'Setting user:'——认证参数有问题");
    assert!(seen_lwjgl, "游戏没加载 LWJGL——classpath/natives 有问题");
}

/// 日志配置文件在版本目录里的落点（安装流程下载的）。
fn logging_file(installer: &Installer, id: &str, merged: &VersionJson) -> Option<PathBuf> {
    let logging = merged.logging.as_ref()?.client.as_ref()?;
    let p = installer.version_dir(id).join(&logging.file.id);
    p.is_file().then_some(p)
}

fn java_path() -> PathBuf {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let p =
            PathBuf::from(home)
                .join("bin")
                .join(if cfg!(windows) { "java.exe" } else { "java" });
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(if cfg!(windows) { "java.exe" } else { "java" })
}
