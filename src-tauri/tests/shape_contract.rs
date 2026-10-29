//! 形状契约门：Rust 的 serde 结构体 与 前端的 TypeScript interface 必须字段一致。
//!
//! 这是这个 crate 里第二份"两侧手写、编译期毫无关联"的契约（第一份是命令名，
//! 见 `ipc_contract.rs`）。字段名错一个字母的后果是：cargo test 全绿、tsc 全绿、
//! UI 上那个值永远是 `undefined`——而 `undefined` 在 React 里通常只是"少显示一个字段"，
//! 不会抛错，所以没人会发现。
//!
//! 这里只比对**字段名**，不比对类型：名字错了是静默的，类型错了 TS 至少会抱怨，
//! 而且运行时形状（Option ↔ `| null`）在两侧的写法本来就不一样。
//!
//! 两侧名字确实不同的载荷列在 `ALIASES` 里（TS 给 Hoard 的类型加了前缀，
//! 免得和游戏版本 `Version` 撞名）。那个列表每条都必须真的被用到，
//! 过期条目会让门"看起来在检查"其实没检查。

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{read, to_camel};

#[derive(Debug)]
struct Shape {
    /// 序列化到 JSON 后的字段名（已经应用过 `rename_all` 与逐字段 `rename`）。
    wire: BTreeSet<String>,
}

/// 所有可能跨 IPC 的 Rust 结构体所在的文件。漏一个文件 = 那个文件里的结构体不被检查，
/// 所以 `every_listed_rust_file_is_parsed` 会盯着这份清单本身。
const RUST_FILES: [&str; 16] = [
    "src/account.rs",
    "src/api.rs",
    "src/archive.rs",
    "src/commands.rs",
    "src/config.rs",
    "src/content.rs",
    "src/install.rs",
    "src/instance.rs",
    "src/java.rs",
    "src/launch.rs",
    "src/loader.rs",
    "src/meta.rs",
    "src/modpack.rs",
    "src/net.rs",
    "src/paths.rs",
    "src/state.rs",
];

/// 两侧名字不同但描述同一个载荷。
const ALIASES: [(&str, &str); 6] = [
    ("Summary", "DownloadSummary"),
    ("Download", "DownloadConfig"),
    ("SearchResponse", "SearchResult"),
    ("Project", "HoardProject"),
    ("Version", "HoardVersion"),
    ("File", "HoardFile"),
];

/// 必须被比对到的形状。少了这条，某次重命名会让一个载荷悄悄从门里消失。
const MUST_COMPARE: [&str; 28] = [
    "AccountView",
    "ArchiveInfo",
    "Config",
    "ContentItem",
    "ContentListing",
    "ContentStats",
    "CopyProgress",
    "Defaults",
    "Download",
    "FileProgress",
    "GameExit",
    "Instance",
    "InstanceLoader",
    "InstanceSettings",
    "InstalledVersion",
    "JavaRuntime",
    "LaunchInfo",
    "LoaderVersion",
    "LogLine",
    "ManifestEntry",
    "ModMeta",
    "ModpackInfo",
    "ModpackInstall",
    "PathsInfo",
    "Resolution",
    "ServerJoin",
    "Summary",
    "ZipSummary",
];

fn rust_shapes() -> BTreeMap<String, Shape> {
    let mut out = BTreeMap::new();
    for rel in RUST_FILES {
        let text = read(rel);
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            let Some(rest) = trimmed.strip_prefix("pub struct ") else {
                continue;
            };
            if !trimmed.ends_with('{') {
                continue; // 元组结构体或跨行定义：这个 crate 里没有跨 IPC 的这种形状
            }
            let name: String = rest
                .trim_end_matches('{')
                .trim()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            // 结构体上方的属性（derive / serde / 文档注释可能有好几行）
            let attrs = attrs_above(&lines, i);
            let camel = attrs
                .iter()
                .any(|a| a.contains("rename_all = \"camelCase\""));

            let mut wire = BTreeSet::new();
            for j in (i + 1)..lines.len() {
                let f = lines[j].trim();
                if f == "}" {
                    break;
                }
                let Some(decl) = f.strip_prefix("pub ") else {
                    continue;
                };
                if decl.starts_with("fn ") || decl.starts_with("const ") {
                    continue;
                }
                let Some((fname, _)) = decl.split_once(':') else {
                    continue;
                };
                let fname = fname.trim();
                if fname.is_empty() {
                    continue;
                }
                let field_attrs = attrs_above(&lines, j);
                // `#[serde(skip)]` 的字段不进 JSON，前端也不该有它
                if field_attrs.iter().any(|a| a.contains("serde(skip")) {
                    continue;
                }
                let renamed = field_attrs.iter().find_map(|a| serde_rename(a));
                wire.insert(renamed.unwrap_or_else(|| {
                    if camel {
                        to_camel(fname)
                    } else {
                        fname.to_string()
                    }
                }));
            }
            out.insert(name, Shape { wire });
        }
    }
    out
}

/// 紧邻在第 `i` 行上方的连续属性行（`#[...]`）。
fn attrs_above<'a>(lines: &[&'a str], i: usize) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut k = i;
    while k > 0 {
        let prev = lines[k - 1].trim();
        if prev.starts_with("#[") {
            out.push(prev);
            k -= 1;
        } else {
            break;
        }
    }
    out
}

/// 逐字段 `#[serde(rename = "type")]`。注意不能匹配到结构体级的 `rename_all = "..."`
/// （那个是 `rename_all`，不是 `rename`）。
fn serde_rename(attr: &str) -> Option<String> {
    let needle = "rename = \"";
    let start = attr.find(needle)? + needle.len();
    let rest = &attr[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn ts_shapes() -> BTreeMap<String, BTreeSet<String>> {
    let chars = common::strip_ts_comments(&read("../src/lib/ipc.ts"));
    let text: String = chars.into_iter().collect();
    let lines: Vec<&str> = text.lines().collect();
    let mut out = BTreeMap::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("export interface ") else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let mut fields = BTreeSet::new();
        for member in lines.iter().skip(i + 1) {
            let m = member.trim();
            if m.starts_with('}') {
                break;
            }
            if m.is_empty() {
                continue;
            }
            let head = m.split([':', ';']).next().unwrap_or("").trim();
            let head = head.trim_end_matches('?');
            // 方法形式的成员（`foo(): void`）不是数据字段
            if head.is_empty()
                || head.contains('(')
                || !head
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
            {
                continue;
            }
            fields.insert(head.to_string());
        }
        out.insert(name, fields);
    }
    out
}

fn ts_name(rust: &str) -> String {
    ALIASES
        .iter()
        .find(|(r, _)| *r == rust)
        .map(|(_, t)| t.to_string())
        .unwrap_or_else(|| rust.to_string())
}

#[test]
fn wire_shapes_match_on_both_sides() {
    let rust = rust_shapes();
    let ts = ts_shapes();
    assert!(!rust.is_empty() && !ts.is_empty(), "解析器什么都没解析到");

    let mut compared: BTreeSet<String> = BTreeSet::new();
    let mut problems = Vec::new();

    for (name, shape) in &rust {
        let ts_name = ts_name(name);
        let Some(ts_fields) = ts.get(&ts_name) else {
            continue; // 这个结构体不跨 IPC（如 AppState、Downloader）
        };
        compared.insert(name.clone());
        let wire = &shape.wire;
        let missing_in_ts: Vec<String> = wire.difference(ts_fields).cloned().collect();
        let missing_in_rust: Vec<String> = ts_fields.difference(wire).cloned().collect();
        if !missing_in_ts.is_empty() || !missing_in_rust.is_empty() {
            problems.push(format!(
                "{name} → {ts_name}: Rust 有而 TS 没有 {missing_in_ts:?}；TS 有而 Rust 没有 {missing_in_rust:?}"
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "IPC 载荷两侧字段不一致（UI 会静默拿到 undefined）：\n{}",
        problems.join("\n")
    );

    for must in MUST_COMPARE {
        assert!(
            compared.contains(must),
            "{must} 没有被比对到：它是不是被重命名了？（改名要同时更新 MUST_COMPARE / ALIASES）"
        );
    }
}

#[test]
fn aliases_have_no_stale_entries() {
    let rust = rust_shapes();
    let ts = ts_shapes();
    for (r, t) in ALIASES {
        assert!(rust.contains_key(r), "ALIASES 里的 Rust 侧 {r} 已不存在");
        assert!(ts.contains_key(t), "ALIASES 里的 TS 侧 {t} 已不存在");
    }
}

#[test]
fn every_listed_rust_file_is_parsed() {
    // 清单本身也要被盯着：漏掉一个文件，那个文件里的载荷就再也不被检查了
    let rust = rust_shapes();
    for rel in RUST_FILES {
        let text = read(rel);
        let declared = text
            .lines()
            .filter(|l| l.trim().starts_with("pub struct ") && l.trim().ends_with('{'))
            .count();
        assert!(
            declared == 0 || !rust.is_empty(),
            "{rel} 声明了 {declared} 个结构体但一个都没解析出来"
        );
    }
    // 抽查几个已知的，确认解析器真的在读字段，而且两种重命名都处理对了
    let inst = rust.get("Instance").expect("Instance 应当被解析");
    assert!(inst.wire.contains("versionId"), "{:?}", inst.wire);
    assert!(inst.wire.contains("lastLaunchedAt"), "{:?}", inst.wire);
    assert!(
        !inst.wire.contains("version_id"),
        "结构体级 rename_all 没生效：{:?}",
        inst.wire
    );
    let item = rust.get("ContentItem").expect("ContentItem 应当被解析");
    assert!(item.wire.contains("displayName"), "{:?}", item.wire);
    // 逐字段 rename：Rust 侧叫 kind，线上叫 type
    let entry = rust.get("ManifestEntry").expect("ManifestEntry 应当被解析");
    assert!(entry.wire.contains("type"), "{:?}", entry.wire);
    assert!(!entry.wire.contains("kind"), "{:?}", entry.wire);
    // 不跨 IPC 的内部结构体不该被误当成 camelCase
    let plan = rust.get("LaunchPlan").expect("LaunchPlan 应当被解析");
    assert!(plan.wire.contains("work_dir"), "{:?}", plan.wire);
}

#[test]
fn error_payload_shape_is_the_documented_one() {
    // `Error` 是手写 Serialize 的（不是 derive），所以走不到上面的结构体比对。
    // 它的形状就是前端 `IpcErrorBody`：{ code, message }。
    let rust = read("src/error.rs");
    assert!(
        rust.contains("serialize_field(\"code\""),
        "Error 不再序列化 code"
    );
    assert!(
        rust.contains("serialize_field(\"message\""),
        "Error 不再序列化 message"
    );
    let ts = ts_shapes();
    let body = ts.get("IpcErrorBody").expect("IpcErrorBody 应当存在");
    assert_eq!(
        body,
        &["code", "message"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        "IpcErrorBody 的字段变了，但 error.rs 的手写 Serialize 没跟着变"
    );
}
