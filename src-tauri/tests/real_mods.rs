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

use compass_lib::content::{self, Kind};

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
            meta.depends
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

        // 依赖列表的不变量：排序 + 去重 + 有上限（三种元数据格式共用同一条规则）
        let mut sorted = meta.depends.clone();
        sorted.sort();
        assert_eq!(sorted, meta.depends, "{}: 依赖没排序", name_of(jar));
        let mut deduped = meta.depends.clone();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            meta.depends.len(),
            "{}: 依赖有重复",
            name_of(jar)
        );
        assert!(meta.depends.len() <= 64, "{}: 依赖超出上限", name_of(jar));

        // 已知样本的具体断言：值来自真 jar 里的真元数据。写成条件断言是为了换样本时
        // 不会假失败，但样本在的时候必须验到具体形状。
        match meta.id.as_str() {
            // sodium 的 fabric.mod.json 里 minecraft 的约束是**数组** ["1.21.4"]，
            // fabricloader 是字符串 ">=0.16.0"，另有一堆 "*"（任意版本，只显示 modId）
            "sodium" => {
                assert!(
                    meta.depends.iter().any(|d| d == "minecraft 1.21.4"),
                    "数组形式的约束没解析出来：{:?}",
                    meta.depends
                );
                assert!(
                    meta.depends.iter().any(|d| d == "fabricloader >=0.16.0"),
                    "{:?}",
                    meta.depends
                );
                assert!(
                    meta.depends
                        .iter()
                        .any(|d| d == "fabric-resource-loader-v0"),
                    "`*` 约束应当只显示 modId：{:?}",
                    meta.depends
                );
                assert!(
                    !meta.depends.iter().any(|d| d.contains('*')),
                    "`*` 不该出现在显示里：{:?}",
                    meta.depends
                );
                // breaks（冲突列表）不是依赖；混进来会给出相反的建议
                assert!(
                    !meta.depends.iter().any(|d| d.contains("optifabric")),
                    "{:?}",
                    meta.depends
                );
            }
            // JEI 的 neoforge/forge mods.toml 把依赖挂在 [[dependencies.<modId>]] 下
            "jei" if meta.loaders.iter().any(|l| l == "neoforge" || l == "forge") => {
                assert!(
                    meta.depends.iter().any(|d| d.starts_with("minecraft ")),
                    "mods.toml 的 dependencies.<modId> 表没被遍历到：{:?}",
                    meta.depends
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
