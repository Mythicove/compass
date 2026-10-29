//! Forge 真装（默认忽略：要联网、要下几百 MB、现代版本还要跑 Java 子进程）。
//!
//! 为什么不能只靠 NeoForge 的 live 测试推过来：Forge 跨了**三套安装格式**，
//! 而 NeoForge 只覆盖其中一套的一半。三份真 installer 的实测差异见
//! `src/forge.rs` 测试模块里的 `FORGE_VERSIONS` 注释。这里真装两个端点：
//! - `1.21.4-54.1.22`：spec 1、client 侧 3 个 processor、5 个带 outputs、
//!   主类 `ForgeBootstrap`、`--launchTarget forge_client`、映射文件扩展名 `@tsrg`。
//! - `1.12.2-14.23.5.2864`：**没有 processor**、旧式 `minecraftArguments` +
//!   `--tweakClass`、必须有原版 jar 在 classpath 上。
//!
//! 中间那套（1.16.5：无 sides、内联 Maven 坐标、SpecialSource）由
//! `src/forge.rs` 的离线测试按真 profile 钉住——它的 pipeline 与 1.21.4 同构，
//! 真装一遍要多下 1GB 资源，换不来新的判断。
//!
//! 用法：
//! ```text
//! cargo test --test forge_live -- --ignored --nocapture
//! # 可选：COMPASS_FORGE_DIR / COMPASS_FORGE_LEGACY_DIR=<数据目录>（复用已下载的库与资源）
//! #       COMPASS_FORGE_VERSION=1.21.4-54.1.22、COMPASS_FORGE_GAME=1.21.4
//! #       COMPASS_FORGE_LEGACY_VERSION=1.12.2-14.23.5.2864、COMPASS_FORGE_LEGACY_GAME=1.12.2
//! ```
//! 别加 `--release`：`[profile.release] panic = "abort"` 与测试要的 unwind 冲突。

mod live;

use std::path::PathBuf;

use compass_lib::loader::{self, LoaderKind};
use compass_lib::paths::Paths;
use compass_lib::version::OsInfo;

/// 装现代 Forge（1.21.4）：跑 client 侧 processor，产物落盘，启动计划里不放游戏 jar。
#[tokio::test]
#[ignore = "needs network, ~700MB of downloads and a Java runtime"]
async fn installs_modern_forge_for_real() {
    let game = live::env_or("COMPASS_FORGE_GAME", "1.21.4");
    let data = live::data_dir("COMPASS_FORGE_DIR");
    let paths = Paths::new(data.path().to_path_buf());
    paths.ensure().unwrap();
    let http = live::http();
    let api = live::api(&http);
    let installer = live::installer(&paths, &http);

    // 版本列表：Forge 的编号是 `<游戏版本>-<forge版本>`，与 NeoForge 的约定不同
    let versions = loader::versions(&api, LoaderKind::Forge, &game)
        .await
        .unwrap();
    assert!(!versions.is_empty(), "没列出任何 Forge 版本");
    let prefix = format!("{game}-");
    assert!(
        versions.iter().all(|v| v.version.starts_with(&prefix)),
        "列表里混进了别的游戏版本：{:?}",
        versions
            .iter()
            .take(5)
            .map(|v| &v.version)
            .collect::<Vec<_>>()
    );
    // 元数据的文档顺序不可信（1.16.5 那段末尾是 36.0.0），所以第一个必须是排出来的最新
    assert!(
        versions[0].version > versions[versions.len() - 1].version || versions.len() == 1,
        "列表没排序：{:?}",
        versions
            .iter()
            .take(3)
            .map(|v| &v.version)
            .collect::<Vec<_>>()
    );
    let chosen = live::env_or("COMPASS_FORGE_VERSION", &versions[0].version);
    println!(
        "forge versions for {game}: {} (chosen {chosen})",
        versions.len()
    );

    let installed = loader::install(&installer, &api, LoaderKind::Forge, &game, &chosen, None)
        .await
        .unwrap();
    println!(
        "installed: id={} jar={} main={} java={} downloaded={} skipped={} failed={:?}",
        installed.id,
        installed.jar_version,
        installed.main_class,
        installed.java_major,
        installed.download.downloaded,
        installed.download.skipped,
        installed.download.failed
    );
    assert_eq!(
        installed.id,
        format!(
            "{game}-forge-{}",
            chosen.strip_prefix(&prefix).unwrap_or(&chosen)
        )
    );
    assert_eq!(installed.download.failed, Vec::<String>::new());
    assert_eq!(
        installed.main_class, "net.minecraftforge.bootstrap.ForgeBootstrap",
        "1.21.4 起 Forge 换了主类（不再是 BootstrapLauncher）"
    );
    // 子版本 JSON 里没有 javaVersion，必须从父版本继承来（1.21.4 → 21）
    assert_eq!(installed.java_major, 21, "javaVersion 没从父版本继承");
    assert_eq!(installed.jar_version, game);

    // processor 的产物：client 侧跑 3 个（DOWNLOAD_MOJMAPS → ForgeAutoRenamingTool → binarypatcher）
    let fssep = std::path::MAIN_SEPARATOR_STR;
    let lib = |rel: &str| paths.libraries().join(rel.replace('/', fssep));
    let patched = lib(&format!(
        "net/minecraftforge/forge/{chosen}/forge-{chosen}-client.jar"
    ));
    assert!(
        patched.is_file(),
        "binarypatcher 的产物 {{PATCHED}} 不存在：{}",
        patched.display()
    );
    assert!(
        std::fs::metadata(&patched).unwrap().len() > 1_000_000,
        "补丁 jar 太小，不像打过补丁的完整客户端"
    );
    // `@tsrg` 是 Forge 1.21.4 才有的扩展名（NeoForge 用 @txt）：maven_path 拼错就会找不到
    let mojmaps = lib("net/minecraft/client/1.21.4/client-1.21.4-mappings.tsrg");
    assert!(
        mojmaps.is_file(),
        "DOWNLOAD_MOJMAPS 的产物不在（@tsrg 扩展名处理错了？）：{}",
        mojmaps.display()
    );
    // ForgeAutoRenamingTool 的产物 {MC_OFF}
    let official = lib("net/minecraft/client/1.21.4/client-1.21.4-official.jar");
    assert!(official.is_file(), "MC_OFF 不存在：{}", official.display());
    // BINPATCH 来自 installer jar 内部，要先解出来
    let extracted = live::walkdir_names(&paths.cache().join("forge-extract"));
    assert!(
        extracted.iter().any(|n| n.ends_with(".lzma")),
        "client.lzma 没被解出来：{extracted:?}"
    );
    // outputs 校验过的证据：profile 给了 {PATCHED}/{MOJMAPS}/{MC_OFF} 的期望 sha1，
    // run_processor 跑完就核对，对不上会直接报错——所以能走到这里就说明核对过了。

    // 合并后的版本 JSON：不继承就没有 client jar 与资源索引
    let merged = compass_lib::runner::read_installed_version(&paths, &installed.id).unwrap();
    assert_eq!(merged.inherits_from.as_deref(), Some(game.as_str()));
    assert!(compass_lib::version::loader_provides_game_jar(&merged));
    // 落盘的是**合并后**的库列表（父版本 + 子版本去重，实测 149 条），不是子版本自己那 49 条。
    // 所以这里只钉两件事：子版本的库确实进来了、去重确实发生了。
    // 钉总数的话，父版本哪天多一条库这里就红——那不是回归，是我把偶然值当成了契约。
    assert!(
        merged.libraries.len() > 49,
        "子版本的库没合并进来：{}",
        merged.libraries.len()
    );
    assert!(
        merged
            .libraries
            .iter()
            .any(|l| l.name.starts_with("net.minecraftforge:forge:")),
        "Forge universal 不在合并后的库里"
    );
    let keys: std::collections::HashSet<String> = merged
        .libraries
        .iter()
        .map(|l| compass_lib::version::library_key(&l.name))
        .collect();
    assert_eq!(
        keys.len(),
        merged.libraries.len(),
        "合并后的库列表里有重复的 Maven 身份（BootstrapLauncher 会因此拒绝启动）"
    );

    let plan = build_plan(&paths, &installed.id, "forge-modern");
    let entries = classpath(&plan);
    println!("classpath entries: {}", entries.len());
    assert!(entries.len() > 100, "只有 {} 条", entries.len());
    for e in &entries {
        assert!(PathBuf::from(e).is_file(), "classpath entry missing: {e}");
        assert!(
            !e.ends_with(&format!("{game}.jar")),
            "原版 jar 不该在 classpath 上：{e}"
        );
    }
    // 与 NeoForge **相反**的一处，而且是数据决定的，不是我们选的：
    // NeoForge 的 version.json 根本不列 `net.neoforged:neoforge:*`（fml_loader 自己按坐标
    // 去 libraries 找补丁 jar，我们再放一个就撞自动模块名）；Forge 1.21.4 的 version.json
    // 把 `:universal` 与 `:client` 都列了进去，而它的 jvm 参数里**没有模块路径**
    // （实测只有 1 条 `-Djava.net.preferIPv6Addresses=system`，NeoForge 有 15 条含 `-p`
    // 与 `--add-modules`），全在 classpath 上就不构成模块，也就没有撞名问题。
    // 所以规则只有一条：**JSON 里写了什么就放什么，我们自己什么都不加**。
    assert!(
        entries
            .iter()
            .any(|e| e.contains(&format!("forge-{chosen}-client.jar"))),
        "Forge 的 version.json 列了打过补丁的 :client，它必须在 classpath 上：{entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.contains(&format!("forge-{chosen}-universal.jar"))),
        "universal jar 必须在 classpath 上"
    );
    assert!(
        !plan.args.iter().any(|a| a == "-p" || a == "--add-modules"),
        "Forge 1.21.4 没有模块路径；如果哪天有了，classpath 规则要重新验一遍：{:?}",
        plan.args
    );
    // asm 在子版本自己的库列表里就重复出现了两次（实测），去重必须发生在合并里
    let asm: Vec<&String> = entries
        .iter()
        .filter(|e| e.contains("ow2") && e.contains("asm"))
        .collect();
    let mut asm_sorted = asm.clone();
    asm_sorted.sort();
    asm_sorted.dedup();
    assert_eq!(
        asm.len(),
        asm_sorted.len(),
        "同一个 asm 出现在 classpath 上两次：{asm:?}"
    );
    let lt = plan
        .args
        .iter()
        .position(|a| a == "--launchTarget")
        .expect("--launchTarget 必须在");
    assert_eq!(
        plan.args[lt + 1],
        "forge_client",
        "Forge 的 target 带下划线"
    );
    assert!(
        plan.args
            .iter()
            .any(|a| a == "net.minecraftforge.bootstrap.ForgeBootstrap"),
        "主类不在参数里：{:?}",
        plan.args
    );
    assert!(
        !plan.args.iter().any(|a| a.contains("${")),
        "残留占位符：{:?}",
        plan.args
    );
    println!(
        "modern forge live install ok ({} cp entries)",
        entries.len()
    );
}

/// 装 legacy Forge（1.12.2）：没有 processor，走 launchwrapper + `--tweakClass`，
/// 而且**必须**有原版 jar 在 classpath 上——与现代版本正好相反。
#[tokio::test]
#[ignore = "needs network, ~1GB of downloads (1.12.2 assets)"]
async fn installs_legacy_forge_for_real() {
    let game = live::env_or("COMPASS_FORGE_LEGACY_GAME", "1.12.2");
    let data = live::data_dir("COMPASS_FORGE_LEGACY_DIR");
    let paths = Paths::new(data.path().to_path_buf());
    paths.ensure().unwrap();
    let http = live::http();
    let api = live::api(&http);
    let installer = live::installer(&paths, &http);

    let versions = loader::versions(&api, LoaderKind::Forge, &game)
        .await
        .unwrap();
    let chosen = live::env_or("COMPASS_FORGE_LEGACY_VERSION", &versions[0].version);
    println!(
        "legacy forge for {game}: {} versions (chosen {chosen})",
        versions.len()
    );

    let installed = loader::install(&installer, &api, LoaderKind::Forge, &game, &chosen, None)
        .await
        .unwrap();
    println!(
        "installed: id={} main={} java={} downloaded={} failed={:?}",
        installed.id,
        installed.main_class,
        installed.java_major,
        installed.download.downloaded,
        installed.download.failed
    );
    assert_eq!(
        installed.id,
        format!(
            "{game}-forge-{}",
            chosen.trim_start_matches(&format!("{game}-"))
        )
    );
    assert_eq!(installed.download.failed, Vec::<String>::new());
    assert_eq!(installed.main_class, "net.minecraft.launchwrapper.Launch");
    // 1.12.2 的版本 JSON 没有 javaVersion 字段 → 按 Java 8 处理（不是"随便哪个"）
    assert_eq!(installed.java_major, 8);

    // 没有 processor：不该有 forge-extract 的产物，也不该因此报错
    let fssep = std::path::MAIN_SEPARATOR_STR;
    let universal = paths
        .libraries()
        .join(format!("net/minecraftforge/forge/{chosen}/forge-{chosen}.jar").replace('/', fssep));
    assert!(
        universal.is_file(),
        "universal jar 是普通库，必须下下来：{}",
        universal.display()
    );

    let merged = compass_lib::runner::read_installed_version(&paths, &installed.id).unwrap();
    assert_eq!(merged.inherits_from.as_deref(), Some(game.as_str()));
    assert!(
        !compass_lib::version::loader_provides_game_jar(&merged),
        "launchwrapper 要原版 jar 在 classpath 上，判据不能误判成 loader 自带"
    );
    // 旧式参数字段要合并过来，否则启动时缺 --tweakClass，症状是"起了个原版"
    let args = merged
        .minecraft_arguments
        .as_deref()
        .expect("1.12.2 用 minecraftArguments");
    assert!(args.contains("--tweakClass"), "{args}");
    assert!(args.contains("FMLTweaker"), "{args}");

    let plan = build_plan(&paths, &installed.id, "forge-legacy");
    let entries = classpath(&plan);
    println!("legacy classpath entries: {}", entries.len());
    for e in &entries {
        assert!(PathBuf::from(e).is_file(), "classpath entry missing: {e}");
    }
    // 与现代版本相反：原版 jar **必须**在
    assert!(
        entries
            .iter()
            .any(|e| e.ends_with(&format!("{fssep}{game}.jar"))
                || e.ends_with(&format!("/{game}.jar"))),
        "原版 jar 不在 classpath 上：{entries:?}"
    );
    assert!(
        entries.iter().any(|e| e.contains("launchwrapper")),
        "launchwrapper 不在 classpath 上"
    );
    assert!(
        entries.iter().any(|e| e.contains("forge-")),
        "universal jar 不在 classpath 上"
    );
    // 旧式参数要展开进 args（没有 modern 的 arguments 对象）
    assert!(
        plan.args.iter().any(|a| a == "--tweakClass"),
        "minecraftArguments 没被展开：{:?}",
        plan.args
    );
    assert!(
        plan.args
            .iter()
            .any(|a| a == "net.minecraftforge.fml.common.launcher.FMLTweaker"),
        "{:?}",
        plan.args
    );
    assert!(
        !plan.args.iter().any(|a| a == "--launchTarget"),
        "1.12.2 不该有 --launchTarget：{:?}",
        plan.args
    );
    println!(
        "legacy forge live install ok ({} cp entries)",
        entries.len()
    );
}

fn build_plan(paths: &Paths, version_id: &str, slug: &str) -> compass_lib::launch::LaunchPlan {
    let merged = compass_lib::runner::read_installed_version(paths, version_id).unwrap();
    let inst = compass_lib::instance::Instance {
        slug: slug.into(),
        name: slug.into(),
        version_id: version_id.into(),
        ..Default::default()
    };
    compass_lib::instance::save(paths, &inst).unwrap();
    let account = live::offline_account();
    let os = OsInfo::current();
    let input = compass_lib::runner::LaunchInput {
        instance: &inst,
        account: &account,
        version: &merged,
        os: &os,
        paths,
        default_memory_mb: 4096,
        api_url: "https://mc.example.com",
        java: live::java_path(),
        authlib_jar: None,
        prefetched_meta: None,
    };
    let plan = compass_lib::runner::build(&input).unwrap();
    let natives = compass_lib::runner::natives_to_extract(&input).unwrap();
    let natives_dir = compass_lib::instance::natives_dir(paths, &inst.slug).unwrap();
    compass_lib::launch::extract_natives(&natives, &natives_dir).unwrap();
    plan
}

fn classpath(plan: &compass_lib::launch::LaunchPlan) -> Vec<String> {
    let i = plan
        .args
        .iter()
        .position(|a| a == "-cp")
        .expect("启动参数里必须有 -cp");
    let sep = if cfg!(windows) { ';' } else { ':' };
    plan.args[i + 1].split(sep).map(String::from).collect()
}
