//! 真实 mod jar 的元数据解析（默认忽略：要联网下载样本）。
//!
//! `content.rs` 的单测用的是**我自己造的** jar：那种测试只能证明解析器等于它自己，
//! 证明不了它读得懂真实世界的 mod。这个项目已经为同类错误付过两次账
//! （mirror 的 `signaturePublickey` 被单测固化成 base64(PEM)、1.21.4 的 `--server`），
//! 所以这里留一个能对真样本跑的口子。
//!
//! 用法：
//! ```text
//! COMPASS_REAL_MODS_DIR=/path/to/jars cargo test --test real_mods -- --ignored --nocapture
//! ```
//! 目录里放几个真 mod jar（Fabric / NeoForge / Forge / 1.12 legacy 各一个最好）。
//! 样本获取方式见文件末尾的 `sample_commands`（Modrinth API，不需要密钥）。

use std::path::{Path, PathBuf};

use compass_lib::content::{self, DepKind, Kind};

/// 依赖的可读形式（打印用）。
fn dep_strings(meta: &content::ModMeta) -> Vec<String> {
    meta.deps
        .iter()
        .map(|d| format!("{} {} {}", d.id, d.kind.as_str(), d.constraint))
        .collect()
}

/// 依赖的排序键，与 `content::deps_from` 用的完全一致。
fn dep_keys(meta: &content::ModMeta) -> Vec<(String, DepKind, String)> {
    meta.deps
        .iter()
        .map(|d| (d.id.clone(), d.kind, d.constraint.clone()))
        .collect()
}

/// 找一条特定 (id, kind) 的依赖，返回它的约束原文。
fn constraint_of(meta: &content::ModMeta, id: &str, kind: DepKind) -> Option<String> {
    meta.deps
        .iter()
        .find(|d| d.id == id && d.kind == kind)
        .map(|d| d.constraint.clone())
}

fn sample_dir() -> Option<PathBuf> {
    std::env::var("COMPASS_REAL_MODS_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
}

#[test]
#[ignore = "needs real mod jars; set COMPASS_REAL_MODS_DIR"]
fn parses_real_mod_jars() {
    let Some(dir) = sample_dir() else {
        eprintln!("COMPASS_REAL_MODS_DIR not set — skipping");
        return;
    };
    let jars: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jar"))
        .collect();
    assert!(!jars.is_empty(), "{} 里没有 jar", dir.display());

    let mut seen_loaders = std::collections::BTreeSet::new();
    for jar in &jars {
        let meta = content::read_mod_meta(jar)
            .unwrap_or_else(|| panic!("{} 解析不出元数据", name_of(jar)));
        println!(
            "{:<44} {:<22} {:<14} {:?} deps={:?}",
            name_of(jar),
            meta.name,
            meta.version,
            meta.loaders,
            dep_strings(&meta)
        );
        // 空的 id/version 意味着解析器读到了文件却拿错了字段——那比读不出来更糟，
        // 因为 UI 会显示一个空白名字而不是回落到文件名
        assert!(!meta.id.trim().is_empty(), "{}: mod id 为空", name_of(jar));
        assert!(!meta.name.trim().is_empty(), "{}: 显示名为空", name_of(jar));
        assert!(
            !meta.version.trim().is_empty(),
            "{}: 版本为空（Forge 的 ${{file.jarVersion}} 占位符没回落成功？）",
            name_of(jar)
        );
        assert!(
            !meta.version.contains("${"),
            "{}: 版本还是构建期占位符 {}",
            name_of(jar),
            meta.version
        );
        assert!(!meta.loaders.is_empty(), "{}: 没识别出加载器", name_of(jar));

        // 依赖列表的不变量：排序 + 去重 + 有上限（四种元数据格式共用同一条规则）。
        // 排序键必须与 `deps_from` 用的**同一个**（id, kind, constraint 三元组）：
        // 这里另写一份"按显示字符串排"的比较器，验的就是我那份副本而不是生产那份——
        // kind 的声明序（required < optional < incompatible < embedded）与名字的字母序
        // 正好不同，第一版就这么假红过一次。
        let keys = dep_keys(&meta);
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(sorted, keys, "{}: 依赖没排序", name_of(jar));
        let mut deduped = keys.clone();
        deduped.dedup();
        assert_eq!(deduped.len(), keys.len(), "{}: 依赖有重复", name_of(jar));
        assert!(keys.len() <= 64, "{}: 依赖超出上限", name_of(jar));
        println!("    deps: {:?}", dep_strings(&meta));
        assert!(
            meta.deps.iter().all(|d| !d.id.trim().is_empty()),
            "{}: 有空 id 的依赖：{:?}",
            name_of(jar),
            meta.deps
        );

        // 已知样本的具体断言：值来自真 jar 里的真元数据。写成条件断言是为了换样本时
        // 不会假失败，但样本在的时候必须验到具体形状。
        match meta.id.as_str() {
            // sodium 的 fabric.mod.json 里 minecraft 的约束是**数组** ["1.21.4"]，
            // fabricloader 是字符串 ">=0.16.0"，另有一堆 "*"（任意版本）
            "sodium" => {
                assert_eq!(
                    constraint_of(&meta, "minecraft", DepKind::Required).as_deref(),
                    Some("1.21.4"),
                    "数组形式的约束没解析出来：{:?}",
                    meta.deps
                );
                assert_eq!(
                    constraint_of(&meta, "fabricloader", DepKind::Required).as_deref(),
                    Some(">=0.16.0"),
                    "{:?}",
                    meta.deps
                );
                assert_eq!(
                    constraint_of(&meta, "fabric-resource-loader-v0", DepKind::Required).as_deref(),
                    Some("*"),
                    "约束原文要留着（求值是另一回事，见 check_dependencies）：{:?}",
                    meta.deps
                );
                // breaks 现在**应该**出现，但必须是 incompatible 而不是 required：
                // 混进 required 会给出完全相反的建议（"缺了它"对"装了才炸"）
                assert!(
                    constraint_of(&meta, "optifabric", DepKind::Incompatible).is_some(),
                    "sodium 的 breaks 没被解析成 incompatible：{:?}",
                    meta.deps
                );
                assert!(
                    constraint_of(&meta, "optifabric", DepKind::Required).is_none(),
                    "冲突项被算成了依赖：{:?}",
                    meta.deps
                );
            }
            // JEI 的 neoforge/forge mods.toml 把依赖挂在 [[dependencies.<modId>]] 下
            "jei" if meta.loaders.iter().any(|l| l == "neoforge" || l == "forge") => {
                assert!(
                    constraint_of(&meta, "minecraft", DepKind::Required).is_some(),
                    "mods.toml 的 dependencies.<modId> 表没被遍历到：{:?}",
                    meta.deps
                );
            }
            // 1.12 的 mcmod.info 里 `dependencies` 常常就是空数组（真样本正是 []）。
            // 空是数据本身如此，不是解析失败——所以这里只认 loader，不断言有内容。
            // 第一版按 mod id 匹配，把 legacy 的 JEI 也算进 toml 断言里，于是假失败。
            "jei" => {
                assert_eq!(
                    meta.loaders,
                    vec!["legacy".to_string()],
                    "{:?}",
                    meta.loaders
                );
            }
            _ => {}
        }
        seen_loaders.extend(meta.loaders.iter().cloned());
    }
    println!("loaders seen: {seen_loaders:?}");
}

/// 把真 jar 放进一个实例的 mods/，验证扫描、启停与统计端到端可用。
#[test]
#[ignore = "needs real mod jars; set COMPASS_REAL_MODS_DIR"]
fn scans_a_real_mods_directory() {
    let Some(dir) = sample_dir() else {
        eprintln!("COMPASS_REAL_MODS_DIR not set — skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let game = tmp.path().join(".minecraft");
    std::fs::create_dir_all(game.join("mods")).unwrap();
    let mut imported = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|x| x == "jar") {
            imported.push(p);
        }
    }
    let names = content::import(&game, Kind::Mods, &imported).unwrap();
    assert_eq!(names.len(), imported.len());

    let items = content::scan(&game, Kind::Mods);
    assert_eq!(items.len(), imported.len(), "{:?}", items.len());
    for item in &items {
        println!(
            "{:<44} enabled={} {:<22} {}",
            item.file_name,
            item.enabled,
            item.display_name,
            item.meta
                .as_ref()
                .map(|m| format!("{} {} {:?}", m.id, m.version, m.loaders))
                .unwrap_or_else(|| "(no metadata)".to_string())
        );
        assert!(item.enabled);
        assert!(item.size > 0);
        assert!(!item.display_name.trim().is_empty());
    }
    let stats = content::stats(&items, Kind::Mods);
    assert_eq!(stats.total, items.len());
    assert_eq!(stats.enabled, items.len());
    assert!(
        stats.bytes > 1_000_000,
        "真 mod 加起来不该只有 {} 字节",
        stats.bytes
    );

    // 禁用第一个再扫一遍：它应当还在列表里，只是 enabled=false，且元数据依然解析得出来
    let victim = items[0].file_name.clone();
    let off = content::set_enabled(&game, &victim, false).unwrap();
    assert!(!off.enabled);
    assert_eq!(off.file_name, victim);
    assert!(
        off.meta.is_some(),
        "禁用之后仍然要能看出这是什么 mod（否则列表里是一堆认不出来的文件名）"
    );
    let after = content::scan(&game, Kind::Mods);
    assert_eq!(after.len(), items.len(), "禁用不该让条目消失");
    assert_eq!(
        after.iter().filter(|i| !i.enabled).count(),
        1,
        "只有被禁用的那一个是 off"
    );
    assert!(
        game.join("mods/disabled").join(&victim).is_file(),
        "禁用 = 移到 mods/disabled/（HMCL/PCL 的惯例，手动移动同样生效）"
    );
    content::set_enabled(&game, &victim, true).unwrap();
    assert!(game.join("mods").join(&victim).is_file());
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
}

/// 一整个真整合包的 mods 目录：**"缺依赖"必须一个都不报**。
///
/// 这是 W11 依赖检查的关键性质，也是我造不出来的样本。用的是 Fabulously Optimized
/// 1.21.4 的 46 个 mod（`modpack_live` 装出来的那份），实测其中 **15 个依赖 id
/// 只由嵌套 jar 提供**（fabric-api 的各子模块、sodium 内嵌的 fabric-renderer-api-v1、
/// controlify 内嵌的 yet_another_config_lib_v3…）。少扫一层嵌套就会报 15 个假缺失，
/// 而一个能正常运行的整合包报出任何"缺依赖"都是我们的 bug，不是包的 bug。
///
/// `COMPASS_REAL_MODS_DIR` 给 mods 目录本身或它的父目录（游戏目录）都可以。
#[test]
#[ignore = "needs a real mods directory; set COMPASS_REAL_MODS_DIR"]
fn a_real_modpack_reports_no_false_missing_dependencies() {
    let Some(dir) = sample_dir() else {
        eprintln!("COMPASS_REAL_MODS_DIR not set — skipping");
        return;
    };
    // scan() 要的是游戏目录（它自己拼 mods/）；给的是 mods 目录时往上退一层
    let game = if dir.file_name().is_some_and(|n| n == "mods") {
        dir.parent().unwrap_or(&dir).to_path_buf()
    } else {
        dir.clone()
    };
    let items = content::scan(&game, Kind::Mods);
    let with_meta: Vec<_> = items.iter().filter(|i| i.meta.is_some()).collect();
    println!(
        "scanned {} items, {} with metadata",
        items.len(),
        with_meta.len()
    );
    assert!(
        with_meta.len() >= 20,
        "样本太小（{} 个有元数据），验不出嵌套 jar 那一层",
        with_meta.len()
    );

    // 先确认解析器真的读到了各类依赖，否则"没有缺失"可能只是因为什么都没解析出来
    let mut required = 0usize;
    let mut optional = 0usize;
    let mut incompatible = 0usize;
    let mut embedded = 0usize;
    for item in &with_meta {
        for d in &item.meta.as_ref().unwrap().deps {
            match d.kind {
                compass_lib::content::DepKind::Required => required += 1,
                compass_lib::content::DepKind::Optional => optional += 1,
                compass_lib::content::DepKind::Incompatible => incompatible += 1,
                compass_lib::content::DepKind::Embedded => embedded += 1,
            }
        }
    }
    println!(
        "deps: required={required} optional={optional} incompatible={incompatible} embedded={embedded}"
    );
    assert!(required > 50, "required 只有 {required}，解析大概退化了");
    assert!(
        incompatible >= 20,
        "incompatible 只有 {incompatible}：实测 21 个 jar 有 breaks、4 个有 conflicts"
    );
    assert!(
        embedded >= 90,
        "embedded 只有 {embedded}：实测这个包有 109 个嵌套 jar"
    );

    // 关键断言：把每个 mod 轮流当"待装的那个"，一个假缺失都不该出现
    let mut missing: Vec<String> = Vec::new();
    let mut blocked: Vec<String> = Vec::new();
    let mut uncertain = 0usize;
    for item in &with_meta {
        let meta = item.meta.as_ref().unwrap();
        let r = content::check_dependencies(
            &items,
            meta,
            Some(compass_lib::loader::LoaderKind::Fabric),
        );
        for m in &r.missing {
            missing.push(format!("{} 需要 {}", meta.id, m.id));
        }
        for b in &r.blocked {
            blocked.push(format!("{} 与 {} 无条件冲突", meta.id, b.id));
        }
        uncertain += r.uncertain.len();
    }
    // 带版本约束的不兼容我们不求值，报多少都合理（实测这个包里确实有）
    println!("uncertain (version-constrained, not evaluated): {uncertain}");
    assert!(
        missing.is_empty(),
        "在能正常运行的整合包上报了 {} 个假缺失：\n{}",
        missing.len(),
        missing.join("\n")
    );
    assert!(
        blocked.is_empty(),
        "在能正常运行的整合包上报了 {} 个假冲突：\n{}",
        blocked.len(),
        blocked.join("\n")
    );
}

/// 取样本的命令（Modrinth 公开 API，不需要密钥）。放在这里是为了下一次能复现：
/// 一个"要联网的测试"如果没人知道样本从哪来，它就等于不存在。
#[allow(dead_code)]
const SAMPLE_COMMANDS: &str = r#"
# PowerShell
$dir = "D:\Temp\opencode\realmods"; New-Item -ItemType Directory -Force -Path $dir | Out-Null
function Get-Mod($project, $loaders, $gvs) {
  $u = "https://api.modrinth.com/v2/project/$project/version?loaders=$([uri]::EscapeDataString($loaders))&game_versions=$([uri]::EscapeDataString($gvs))"
  $v = (Invoke-RestMethod -Uri $u -TimeoutSec 30 | Select-Object -First 1)
  $f = ($v.files | Where-Object { $_.primary } | Select-Object -First 1); if (-not $f) { $f = $v.files[0] }
  Invoke-WebRequest -Uri $f.url -OutFile "$dir\$($f.filename)" -TimeoutSec 180
}
Get-Mod "sodium"  '["fabric"]'   '["1.21.4"]'   # fabric.mod.json
Get-Mod "lithium" '["fabric"]'   '["1.20.1"]'   # fabric.mod.json
Get-Mod "jei"     '["neoforge"]' '["1.21.4"]'   # META-INF/neoforge.mods.toml
Get-Mod "jei"     '["forge"]'    '["1.20.1"]'   # META-INF/mods.toml + ${file.jarVersion}
Get-Mod "jei"     '["forge"]'    '["1.12.2"]'   # mcmod.info
"#;
