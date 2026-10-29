//! 真装一个 `.mrpack`（默认忽略：要联网，会下 ~50MB 的 mod + 一整套 1.21.4）。
//!
//! 用的是仓库里那份真包（Modrinth 上 Fabulously Optimized 8.2.0 / 1.21.4），
//! 不是自己造的样本：造出来的包只会验到"我以为的格式"。
//!
//! 用法：
//! ```text
//! cargo test --test modpack_live -- --ignored --nocapture
//! # 可选：COMPASS_MODPACK_DIR=<数据目录>（复用已下载的原版库与资源）
//! ```
//! 别加 `--release`：`[profile.release] panic = "abort"` 与测试要的 unwind 冲突。

mod live;

use std::path::{Path, PathBuf};

use compass_lib::instance;
use compass_lib::modpack;
use compass_lib::paths::Paths;
use compass_lib::version::OsInfo;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mrpack-fabulously-optimized.mrpack")
}

#[tokio::test]
#[ignore = "needs network: ~50MB of mods plus a full 1.21.4 install"]
async fn installs_a_real_mrpack_for_real() {
    let data = live::data_dir("COMPASS_MODPACK_DIR");
    let paths = Paths::new(data.path().to_path_buf());
    paths.ensure().unwrap();
    // 可重复运行：上一次装出来的实例先删掉，否则第二次直接撞 "instance already exists"
    if instance::config_file(&paths, "fo-live")
        .map(|f| f.exists())
        .unwrap_or(false)
    {
        instance::remove(&paths, "fo-live").unwrap();
    }
    let http = live::http();
    let api = live::api(&http);
    let installer = live::installer(&paths, &http);

    // 先看后装：这一步本身就是流程的一部分，不是可选预览
    let info = modpack::inspect(&fixture()).unwrap();
    println!(
        "inspect: {} pack={} mc={:?} loader={:?} {:?} files={} (req={} opt={} skip={}) overrides={} bytes={}",
        info.name,
        info.pack_version,
        info.minecraft,
        info.loader,
        info.loader_version,
        info.files_total,
        info.files_required,
        info.files_optional,
        info.files_skipped,
        info.override_entries,
        info.bytes
    );
    assert_eq!(info.minecraft.as_deref(), Some("1.21.4"));
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);

    let started = std::time::Instant::now();
    let res = modpack::install(
        &paths,
        &api,
        &installer,
        &fixture(),
        "fo-live",
        "FO live",
        None,
    )
    .await
    .unwrap();
    println!(
        "installed in {:.0}s: version={} downloaded={} bytes={} optional={} skipped={} overrides={} overwritten={:?} failed={:?} warnings={:?}",
        started.elapsed().as_secs_f64(),
        res.instance.version_id,
        res.downloaded,
        res.bytes,
        res.optional_installed,
        res.skipped_unsupported,
        res.override_files,
        res.overwritten,
        res.failed,
        res.warnings
    );

    assert_eq!(res.instance.slug, "fo-live");
    assert_eq!(res.instance.name, "FO live");
    // loader 装完后实例必须指向 loader 的继承版本 id，那才是启动时用的版本
    assert_eq!(
        res.instance.version_id, "fabric-loader-0.19.3-1.21.4",
        "{}",
        res.instance.version_id
    );
    let loader = res.instance.loader.as_ref().expect("loader 应当被记录下来");
    assert_eq!(loader.version, "0.19.3");
    assert_eq!(loader.game_version, "1.21.4");

    assert_eq!(res.failed, Vec::<String>::new(), "有文件没下下来");
    assert!(res.warnings.is_empty(), "{:?}", res.warnings);
    assert_eq!(res.downloaded, 48, "48 个文件都该到位");
    assert_eq!(res.override_files, info.override_entries);
    assert_eq!(res.skipped_unsupported, 0);
    assert!(res.bytes > 10_000_000, "{}", res.bytes);

    // 落点：46 个 mod + 2 个资源包，都在游戏目录里对应的子目录
    let game = instance::game_dir(&paths, "fo-live").unwrap();
    let mods = live::walkdir_names(&game.join("mods"));
    let packs = live::walkdir_names(&game.join("resourcepacks"));
    // 每个索引声明的文件都必须在它声明的位置上。这比数子目录里有几个文件强：
    // 那个数目同时来自索引与 overrides（实测 mods/ 46 个全来自索引，
    // resourcepacks/ 3 个里有 1 个来自 overrides），写死就是把两个来源的
    // 偶然之和当成了契约。
    let index = modpack::read_index(&fixture()).unwrap();
    for f in &index.files {
        let p = game.join(f.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert!(
            p.is_file(),
            "索引声明的文件不在：{} -> {}",
            f.path,
            p.display()
        );
    }
    println!(
        "game dir: mods={} resourcepacks={} (index declares {} files)",
        mods.len(),
        packs.len(),
        index.files.len()
    );
    assert!(
        mods.iter().all(|n| n.ends_with(".jar")),
        "mods/ 里出现了不是 jar 的东西：{mods:?}"
    );
    // overrides 里的配置也到位了（这个包的 overrides 是 config/ 与 resourcepacks/）
    assert!(game.join("config").is_dir(), "overrides 没被应用");
    let configs = live::walkdir_names(&game.join("config"));
    assert!(configs.len() > 30, "config 只有 {} 个文件", configs.len());
    // 索引本身不是游戏内容
    assert!(!game.join("modrinth.index.json").exists());

    // 每个 mod 的 sha1 都核对过了（下载器落位前校验），这里再抽查一个：
    // 抽的是索引里第一个文件，路径与 sha1 都取自包本身
    let first = &index.files[0];
    let want = first.hashes.get("sha1").unwrap();
    let got = compass_lib::net::sha1_of_file(&game.join(&first.path))
        .await
        .unwrap();
    assert_eq!(&got, want, "{} 的内容与索引里的 sha1 不符", first.path);

    // 装出来的实例要能拼出启动计划，且 classpath 上每一项都在磁盘上
    let merged =
        compass_lib::runner::read_installed_version(&paths, &res.instance.version_id).unwrap();
    let account = live::offline_account();
    let os = OsInfo::current();
    let input = compass_lib::runner::LaunchInput {
        instance: &res.instance,
        account: &account,
        version: &merged,
        os: &os,
        paths: &paths,
        default_memory_mb: 4096,
        api_url: "https://mc.example.com",
        java: live::java_path(),
        authlib_jar: None,
        prefetched_meta: None,
    };
    let plan = compass_lib::runner::build(&input).unwrap();
    let cp = plan
        .args
        .iter()
        .position(|a| a == "-cp")
        .expect("启动参数里必须有 -cp");
    let sep = if cfg!(windows) { ';' } else { ':' };
    let entries: Vec<&str> = plan.args[cp + 1].split(sep).collect();
    println!("classpath entries: {}", entries.len());
    assert!(entries.len() > 50, "只有 {} 条", entries.len());
    for e in &entries {
        assert!(PathBuf::from(e).is_file(), "classpath entry missing: {e}");
    }
    // Fabric 走 KnotClient，必须有原版 jar（与 NeoForge/Forge 1.17+ 相反）
    assert!(
        entries.iter().any(|e| e.ends_with("1.21.4.jar")),
        "原版 jar 不在 classpath 上：{entries:?}"
    );
    assert!(
        plan.args
            .iter()
            .any(|a| a == "net.fabricmc.loader.impl.launch.knot.KnotClient"),
        "Fabric 主类不在参数里：{:?}",
        plan.args
    );
    println!("mrpack live install ok ({} cp entries)", entries.len());

    // 同一个 slug 再装一次必须被明确拒绝，而不是覆盖掉已装好的实例
    let again = modpack::install(
        &paths,
        &api,
        &installer,
        &fixture(),
        "fo-live",
        "FO live",
        None,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(again.contains("already exists"), "{again}");
}

/// 装失败不能留下半个实例：用户看到缺文件的实例比看到一个明确的失败更难处理。
#[tokio::test]
#[ignore = "needs network for the parent version install"]
async fn a_failed_modpack_install_leaves_no_instance_behind() {
    let data = live::data_dir("COMPASS_MODPACK_DIR");
    let paths = Paths::new(data.path().to_path_buf());
    paths.ensure().unwrap();
    let http = live::http();
    let api = live::api(&http);
    let installer = live::installer(&paths, &http);

    // 造一个装不了的包：索引合法、但依赖的 loader 版本不存在。
    // 用"不存在的 loader 版本"而不是"坏索引"，是因为要验的是**装到一半失败**的回滚，
    // 索引就坏的话连实例目录都不会建，什么也验不到。
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("overrides/config")).unwrap();
    std::fs::write(
        src.join("modrinth.index.json"),
        r#"{"formatVersion":1,"game":"minecraft","versionId":"1","name":"Broken",
            "dependencies":{"minecraft":"1.21.4","fabric-loader":"0.0.0-does-not-exist"},
            "files":[]}"#,
    )
    .unwrap();
    std::fs::write(src.join("overrides/config/x.toml"), b"x").unwrap();
    let pack = dir.path().join("broken.mrpack");
    compass_lib::archive::zip_dir(&src, &pack, &[]).unwrap();

    let err = modpack::install(&paths, &api, &installer, &pack, "broken-pack", "", None)
        .await
        .unwrap_err()
        .to_string();
    println!("failed as expected: {err}");
    assert!(
        !instance::instance_dir(&paths, "broken-pack")
            .map(|d| d.exists())
            .unwrap_or(false),
        "失败的安装留下了实例目录"
    );
    assert!(
        !instance::config_file(&paths, "broken-pack")
            .map(|f| f.exists())
            .unwrap_or(false),
        "失败的安装留下了实例配置"
    );
    assert!(
        !instance::list(&paths)
            .iter()
            .any(|i| i.slug == "broken-pack"),
        "实例列表里出现了装失败的包"
    );
}
