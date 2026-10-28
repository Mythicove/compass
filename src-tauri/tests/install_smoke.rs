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
use compass_lib::loader::LoaderKind;
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
    let (classpath, natives) = split_libraries(&merged, &os, &paths).unwrap();
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
        client_jar: Some(jar.clone()),
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
        extra_game_args: vec![],
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

    // 6. 实例与内容的往返：建实例 → 放内容 → 导出 → 导入 → 复制。
    //    这一段不联网也不启动，所以三平台 CI 每次都会跑到。
    let inst = compass_lib::instance::Instance {
        slug: "smoke".into(),
        name: "Smoke".into(),
        version_id: VERSION.into(),
        memory_mb: Some(2048),
        created_at: 0,
        ..compass_lib::instance::Instance::default()
    };
    compass_lib::instance::save(&paths, &inst).unwrap();

    // ---- 导出 → 导入 → 复制，产物都必须仍然是可启动的实例 ----
    // 真正"能不能启动"由下面的 COMPASS_SMOKE_LAUNCH 分支验证——那里启动的是
    // **导入出来的**实例，因为验收项写的就是"导出再导入的实例可正常启动"。
    let game = compass_lib::instance::game_dir(&paths, "smoke").unwrap();
    std::fs::create_dir_all(game.join("mods")).unwrap();
    std::fs::write(game.join("mods/fake-mod.jar"), b"fake mod").unwrap();
    std::fs::create_dir_all(game.join("saves/w")).unwrap();
    std::fs::write(game.join("saves/w/level.dat"), b"level").unwrap();

    let zip = dir.path().join("smoke-export.zip");
    let summary = compass_lib::instance::export(&paths, "smoke", &zip, true).unwrap();
    assert!(
        summary.files >= 3,
        "instance.json + mod + level.dat 至少三个条目：{summary:?}"
    );
    // 导入前的"看一眼"：只读压缩包，不落盘
    let info = compass_lib::instance::inspect_archive(&zip).unwrap();
    assert_eq!(info.version_id, VERSION);
    assert_eq!(info.files, summary.files);

    let imported_game = compass_lib::instance::game_dir(&paths, "smoke-imported").unwrap();
    let imported = compass_lib::instance::import(&paths, &zip, "smoke-imported", "").unwrap();
    assert_eq!(imported.version_id, VERSION, "导入后版本 id 必须还在");
    assert_eq!(
        std::fs::read(imported_game.join("mods/fake-mod.jar")).unwrap(),
        b"fake mod"
    );
    assert!(imported_game.join("saves/w/level.dat").is_file());
    assert_eq!(
        imported.java_path, None,
        "别人机器上的 Java 路径必须在导入时清空"
    );
    assert_eq!(imported.launches, 0);
    assert_eq!(imported.last_launched_at, None);

    // 复制出来的实例同样自洽（与导入共用同一套跳过规则）
    let copied = compass_lib::instance::duplicate(
        &paths,
        "smoke-imported",
        "smoke-copy",
        "Copy",
        |done, total| {
            assert!(done <= total, "进度不能超过总量：{done}/{total}");
        },
    )
    .unwrap();
    assert_eq!(copied.name, "Copy");
    let copied_game = compass_lib::instance::game_dir(&paths, "smoke-copy").unwrap();
    assert!(copied_game.join("mods/fake-mod.jar").is_file());
    assert!(
        !compass_lib::instance::instance_dir(&paths, "smoke-copy")
            .unwrap()
            .join("natives")
            .exists(),
        "natives 每次启动重建，不该被复制/导出"
    );
    println!(
        "instance round-trip ok: exported {} files ({} bytes) → imported {} → copied {}",
        summary.files, summary.bytes, imported.slug, copied.slug
    );

    // 启动的是导入出来的实例（见上面的注释：那才是验收项）
    let inst = imported;

    // 7. Fabric：真装一个 loader 版本，验"继承型版本"这条链路端到端可用。
    //    这一段不启动游戏，所以 CI 三平台每次都会跑到（启动在第 8 步，需要显示环境）。
    let api = compass_lib::api::ApiClient::new(http.clone());
    let fabric_versions = compass_lib::loader::versions(&api, LoaderKind::Fabric, VERSION)
        .await
        .unwrap();
    assert!(
        !fabric_versions.is_empty(),
        "Fabric meta 对 {VERSION} 没给出任何 loader 版本"
    );
    // stable 优先（Fabric 的 loader 对象有这个字段），没有就退回第一个
    let chosen = fabric_versions
        .iter()
        .find(|v| v.stable == Some(true))
        .unwrap_or(&fabric_versions[0]);
    println!(
        "fabric loader versions: {} (chosen {} stable={:?})",
        fabric_versions.len(),
        chosen.version,
        chosen.stable
    );
    let fabric = compass_lib::loader::install(
        &installer,
        &api,
        LoaderKind::Fabric,
        VERSION,
        &chosen.version,
        None,
    )
    .await
    .unwrap();
    println!(
        "fabric installed: id={} jar={} main={} libs downloaded={} skipped={}",
        fabric.id,
        fabric.jar_version,
        fabric.main_class,
        fabric.download.downloaded,
        fabric.download.skipped
    );
    assert!(
        fabric.id.starts_with("fabric-loader-"),
        "profile 的 id 应当是 loader 生成的形状：{}",
        fabric.id
    );
    assert_eq!(
        fabric.jar_version, VERSION,
        "client jar 来自父版本（继承型版本自己不带 jar）"
    );
    assert!(
        fabric.main_class.contains("fabricmc"),
        "合并后主类应当是 Fabric 的 Knot：{}",
        fabric.main_class
    );
    // 合并后的版本 JSON 落盘 = 可离线启动
    let fabric_json = compass_lib::runner::installed_version_path(&paths, &fabric.id);
    assert!(fabric_json.is_file(), "{} 没落盘", fabric_json.display());
    let merged_fabric = compass_lib::runner::read_installed_version(&paths, &fabric.id).unwrap();
    // 落盘的合并结果**仍然**带着 inheritsFrom：`jar_version()` 靠它定位 client jar
    // （继承型版本自己不带 jar，jar 在父版本目录下）。清掉它，启动就会报 client jar missing。
    assert_eq!(
        merged_fabric.inherits_from.as_deref(),
        Some(VERSION),
        "合并结果应当仍指向提供 jar 的父版本"
    );
    assert_eq!(merged_fabric.jar_version(), VERSION);

    // 启动计划：这一步验的正是 P1 那条教训——子版本的 arguments.jvm 只有一条
    // `-DFabricMcEmu=…`，按"子覆盖父"合并会让命令行里既没有 -Djava.library.path 也没有 -cp。
    let fabric_inst = compass_lib::instance::Instance {
        slug: "smoke-fabric".into(),
        name: "Smoke Fabric".into(),
        version_id: fabric.id.clone(),
        memory_mb: Some(2048),
        ..compass_lib::instance::Instance::default()
    };
    compass_lib::instance::save(&paths, &fabric_inst).unwrap();
    let fake_account = compass_lib::account::Account {
        player_name: "SmokeTester".into(),
        player_uuid: "069a79f444e94726a5befca90e38aaf5".into(),
        access_token: "0".repeat(32),
        client_token: "0".repeat(32),
        api_url: "https://mc.example.com".into(),
        signed_in_at: 0,
    };
    let plan = compass_lib::runner::build(&compass_lib::runner::LaunchInput {
        instance: &fabric_inst,
        account: &fake_account,
        version: &merged_fabric,
        os: &OsInfo::current(),
        paths: &paths,
        default_memory_mb: 2048,
        api_url: "https://mc.example.com",
        java: java_path(),
        authlib_jar: None,
        prefetched_meta: None,
    })
    .unwrap();
    assert!(
        plan.args.iter().any(|a| a.contains("KnotClient")),
        "主类应当是 Fabric 的 KnotClient：{:?}",
        plan.args
    );
    assert!(
        plan.args.iter().any(|a| a.starts_with("-DFabricMcEmu=")),
        "子版本的 JVM 参数丢了：{:?}",
        plan.args
    );
    assert!(
        plan.args
            .iter()
            .any(|a| a.starts_with("-Djava.library.path=")),
        "父版本的 JVM 参数被子版本覆盖了（P1 修过的正是这个）：{:?}",
        plan.args
    );
    let cp_idx = plan
        .args
        .iter()
        .position(|a| a == "-cp")
        .expect("合并后必须有 -cp");
    let cp = &plan.args[cp_idx + 1];
    assert!(
        cp.contains("fabric-loader"),
        "classpath 里应当有 fabric-loader：{cp}"
    );
    assert!(
        cp.contains("intermediary"),
        "classpath 里应当有 intermediary：{cp}"
    );
    // classpath 上每一项都得真在磁盘上（下载少一个 jar，JVM 抛的是 ClassNotFoundException）
    let sep = if cfg!(windows) { ';' } else { ':' };
    for entry in cp.split(sep) {
        assert!(
            PathBuf::from(entry).is_file(),
            "classpath entry missing: {entry}"
        );
    }
    assert!(
        !plan.args.iter().any(|a| a.contains("${")),
        "unsubstituted placeholder: {:?}",
        plan.args
    );

    // 8. Quilt：同一个实现、另一个 meta 服务。这里只验端点与 profile 形状，
    //    不重复一遍完整安装（下载与合并的代码路径与 Fabric 完全相同）。
    let quilt_versions = compass_lib::loader::versions(&api, LoaderKind::Quilt, VERSION)
        .await
        .unwrap();
    assert!(
        !quilt_versions.is_empty(),
        "Quilt meta 对 {VERSION} 没给出任何 loader 版本"
    );
    // Quilt 的 loader 对象**没有** stable 字段：必须是 None，不能是 false
    // （否则 UI 会把 Quilt 的每个版本都标成"开发版"，那是替它编了一句它没说过的话）
    assert!(
        quilt_versions.iter().all(|v| v.stable.is_none()),
        "Quilt 的 stable 应当全是 None：{:?}",
        quilt_versions.iter().take(3).collect::<Vec<_>>()
    );
    let quilt_profile =
        compass_lib::loader::profile(&api, LoaderKind::Quilt, VERSION, &quilt_versions[0].version)
            .await
            .unwrap();
    assert_eq!(
        quilt_profile.inherits_from.as_deref(),
        Some(VERSION),
        "Quilt profile 的父版本应当是游戏版本"
    );
    assert!(
        quilt_profile.id.starts_with("quilt-loader-"),
        "{}",
        quilt_profile.id
    );
    assert!(
        quilt_profile
            .main_class
            .as_deref()
            .is_some_and(|m| m.contains("quiltmc")),
        "主类应当是 Quilt 的 Knot：{:?}",
        quilt_profile.main_class
    );
    println!(
        "quilt ok: {} loader versions, profile {} (libs {})",
        quilt_versions.len(),
        quilt_profile.id,
        quilt_profile.libraries.len()
    );

    // 9. 可选：真的把游戏拉起来，启动的是**导入出来的**实例（验收项：导出再导入可正常启动）。
    //    走 runner::prepare_launch——与 instance_launch 命令是同一条代码路径，
    //    所以这里验的就是用户会跑到的东西（而不是测试自己重写一遍顺序）。
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
