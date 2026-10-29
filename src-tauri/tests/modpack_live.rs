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

    // 装完的体检：一个能正常运行的整合包不该报出任何缺失或不兼容。
    // 这条与 real_mods 里那条同源，但走的是完整安装路径（含 loader 记录 →
    // platform_ids 的推导），所以 fabricloader / minecraft 这些 id 也必须被认成"已提供"。
    println!(
        "dep check: missing={} blocked={} uncertain={}",
        res.deps.missing.len(),
        res.deps.blocked.len(),
        res.deps.uncertain.len()
    );
    assert!(
        res.deps.missing.is_empty(),
        "在能正常运行的整合包上报了假缺失：{:?}",
        res.deps.missing
    );
    assert!(
        res.deps.blocked.is_empty(),
        "在能正常运行的整合包上报了假冲突：{:?}",
        res.deps.blocked
    );
    // 带版本约束的不兼容我们不求值，这个包里确实有（实测 38 条），报多少都合理
    assert!(
        !res.deps.uncertain.is_empty(),
        "这个包里有带版本区间的 breaks，一条都没查出来说明解析退化了"
    );

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

/// 往返：装一个真包 → 导出成 .mrpack → 把导出的包再装成一个新实例。
///
/// 这一条钉的是**导出物本身可安装**，而不只是"能写出一个 zip"。
/// 关键是体积：48 个 mod 共 42MB，而导出物应当只有几百 KB——有来源簿记的文件
/// 写成了 `files[]`（地址 + sha1），只有配置按字节进 `overrides/`。
/// 簿记要是没生效，这里会是一个 42MB 的包，断言会立刻指出来。
#[tokio::test]
#[ignore = "needs network: installs the pack, then re-installs the exported one"]
async fn an_exported_pack_installs_back() {
    let data = live::data_dir("COMPASS_MODPACK_DIR");
    let paths = Paths::new(data.path().to_path_buf());
    paths.ensure().unwrap();
    let http = live::http();
    let api = live::api(&http);
    let installer = live::installer(&paths, &http);

    // 源实例必须**新装**：来源簿记是安装时写下的，复用一个早于簿记功能装出来的
    // 实例会得到 files=0（第一版就这么假红过——那不是导出的 bug，是样本不对）。
    // 用自己的 slug，不依赖另一条测试留下的状态。
    for slug in ["fo-export-src", "fo-roundtrip"] {
        if instance::config_file(&paths, slug)
            .map(|f| f.exists())
            .unwrap_or(false)
        {
            instance::remove(&paths, slug).unwrap();
        }
    }
    modpack::install(
        &paths,
        &api,
        &installer,
        &fixture(),
        "fo-export-src",
        "FO export source",
        None,
    )
    .await
    .expect("源实例应当装得起来（详细输出见 installs_a_real_mrpack_for_real）");

    let dest = data.path().join("exported.mrpack");
    let _ = std::fs::remove_file(&dest);
    let out = modpack::export(&paths, "fo-export-src", &dest, "8.2.0-reexport")
        .await
        .unwrap();
    let pack_size = std::fs::metadata(&dest).unwrap().len();
    println!(
        "exported: files={} overrides={} bytes={} stale={:?} skipped={:?} pack={}KB",
        out.files,
        out.override_files,
        out.bytes,
        out.stale,
        out.skipped_dirs,
        pack_size / 1024
    );
    // 48 个 mod 全都有来源簿记（它们是 modpack::install 下的），所以全进 files[]
    assert_eq!(out.files, 48, "{out:?}");
    assert!(
        out.stale.is_empty(),
        "刚装完不该有过期簿记：{:?}",
        out.stale
    );
    assert!(out.override_files > 30, "{out:?}");
    // 没启动过的实例不会有 logs/，所以只在它存在时要求被报告为跳过
    // （精确的排除行为由离线测试 export_splits_known_files_from_the_rest 钉）
    let game_src = instance::game_dir(&paths, "fo-export-src").unwrap();
    if game_src.join("logs").is_dir() {
        assert!(
            out.skipped_dirs.iter().any(|d| d == "logs"),
            "日志目录存在就该被报告为跳过：{:?}",
            out.skipped_dirs
        );
    }
    // 体积是这条测试的重点：42MB 的 mod 变成了几百 KB 的索引
    assert!(
        pack_size < 4 * 1024 * 1024,
        "导出物 {}KB：来源簿记大概没生效（48 个 mod 共 42MB 全按字节打包了）",
        pack_size / 1024
    );

    // 导出的包要能被我们自己的 inspect 读回来，形状与来源包一致
    let info = modpack::inspect(&dest).unwrap();
    println!(
        "re-inspect: {} pack={} mc={:?} loader={:?} {:?} files={} overrides={}",
        info.name,
        info.pack_version,
        info.minecraft,
        info.loader,
        info.loader_version,
        info.files_total,
        info.override_entries
    );
    assert_eq!(info.pack_version, "8.2.0-reexport");
    assert_eq!(info.minecraft.as_deref(), Some("1.21.4"));
    assert_eq!(
        info.loader,
        Some(compass_lib::loader::LoaderKind::Fabric),
        "loader 依赖要从实例记录写进索引，否则装回来就是原版"
    );
    assert_eq!(info.loader_version.as_deref(), Some("0.19.3"));
    assert_eq!(info.files_total, 48);
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);

    if instance::config_file(&paths, "fo-roundtrip")
        .map(|f| f.exists())
        .unwrap_or(false)
    {
        instance::remove(&paths, "fo-roundtrip").unwrap();
    }
    let back = modpack::install(&paths, &api, &installer, &dest, "fo-roundtrip", "", None)
        .await
        .unwrap();
    println!(
        "re-installed: version={} name={} downloaded={} overrides={} failed={:?} missing={} blocked={}",
        back.instance.version_id,
        back.instance.name,
        back.downloaded,
        back.override_files,
        back.failed,
        back.deps.missing.len(),
        back.deps.blocked.len()
    );
    assert_eq!(back.instance.version_id, "fabric-loader-0.19.3-1.21.4");
    assert_eq!(back.failed, Vec::<String>::new());
    assert_eq!(back.downloaded, 48, "48 个文件都该重新下下来");
    assert!(back.deps.missing.is_empty(), "{:?}", back.deps.missing);
    assert!(back.deps.blocked.is_empty(), "{:?}", back.deps.blocked);
    // 名字留空时用包里的名字
    assert_eq!(back.instance.name, info.name);

    // 落点与来源一致：索引声明的每个文件都在
    let game = instance::game_dir(&paths, "fo-roundtrip").unwrap();
    for f in &info_files(&dest) {
        let p = game.join(f.replace('/', std::path::MAIN_SEPARATOR_STR));
        assert!(p.is_file(), "往返之后少了 {f}");
    }
}

/// 导出包里索引声明的路径列表。
fn info_files(pack: &Path) -> Vec<String> {
    modpack::read_index(pack)
        .unwrap()
        .files
        .into_iter()
        .map(|f| f.path)
        .collect()
}
