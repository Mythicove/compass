//! IPC 契约门：三份手写清单必须一致。
//!
//! - `src-tauri/src/commands.rs` 里 `pub async fn` 定义的命令
//! - `src-tauri/src/lib.rs` 的 `generate_handler!` 里注册的命令
//! - `src/lib/ipc.ts` 里前端实际 `invoke` 的命令
//!
//! 三份清单没有任何编译期关联，所以漂移的症状是"编译通过、cargo test 通过、
//! 点了按钮才发现 command not found"。P1 结束时这道门第一次跑就查出两个
//! 定义了但没人注册也没人调的命令（`auth_status`、`instance_ready`）——
//! 它们既没被 UI 用过，也就从来没被测过。
//!
//! 这里同时要求 **每个命令都被前端调用**：启动器没有别的消费者，
//! 一个没有调用方的命令就是一个没有验证过的命令。要么这一步就接上 UI，
//! 要么先别加（等它的使用者一起落地）。

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// `pub async fn NAME(` 里的 NAME。
fn defined_commands() -> BTreeSet<String> {
    read("src/commands.rs")
        .lines()
        .filter_map(|l| l.split("pub async fn ").nth(1))
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|n| !n.is_empty())
        .collect()
}

/// `commands::NAME` 里的 NAME。
fn registered_commands() -> BTreeSet<String> {
    read("src/lib.rs")
        .lines()
        .filter_map(|l| l.split("commands::").nth(1))
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|n| !n.is_empty())
        .collect()
}

/// `invoke<T>("NAME"` 里的 NAME。
///
/// 只认**调用形式**：`invoke` 后面（跳过空白与配平的泛型尖括号）必须紧跟 `(`，
/// 再跟一个字符串字面量。否则 `import { invoke } from "@tauri-apps/api/core"`
/// 这种地方会被当成一次调用——第一版就是这么错的，抓出来两个不存在的命令名。
/// 注释先整体剥掉：被注释掉的调用不算调用方。全文扫描而非逐行，
/// 泛型或参数换行的写法同样是合法调用，不该让这道门误报。
fn invoked_commands() -> BTreeSet<String> {
    parse_invoked(&read("../src/lib/ipc.ts"))
}

/// 解析器单独可测：一道会误报的门很快就会被下一个人删掉，
/// 一道会漏报的门比没有门更糟（绿灯变成假信号）。
fn parse_invoked(text: &str) -> BTreeSet<String> {
    let chars = strip_comments(text);
    let mut out = BTreeSet::new();
    let mut i = 0;
    while let Some(found) = find_from(&chars, "invoke", i) {
        i = found + "invoke".len();
        // 前一个字符不能是标识符字符（否则会匹配到 `invoked` 之类）
        if found > 0 && (chars[found - 1].is_alphanumeric() || chars[found - 1] == '_') {
            continue;
        }
        let mut j = i;
        skip_ws(&chars, &mut j);
        if j < chars.len() && chars[j] == '<' && !skip_balanced(&chars, &mut j, '<', '>') {
            continue;
        }
        skip_ws(&chars, &mut j);
        if j >= chars.len() || chars[j] != '(' {
            continue;
        }
        j += 1;
        skip_ws(&chars, &mut j);
        if j >= chars.len() || chars[j] != '"' {
            continue;
        }
        j += 1;
        let mut name = String::new();
        while j < chars.len() && chars[j] != '"' {
            name.push(chars[j]);
            j += 1;
        }
        if !name.is_empty() {
            out.insert(name);
        }
        i = j;
    }
    out
}

/// 去掉 `//` 行注释与 `/* */` 块注释。字符串与模板字面量里的 `//` 不算注释
/// （否则 URL 会把后面的代码整段吃掉）。
fn strip_comments(text: &str) -> Vec<char> {
    let src: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            out.push(c);
            i += 1;
            while i < src.len() {
                out.push(src[i]);
                if src[i] == '\\' {
                    i += 1;
                    if i < src.len() {
                        out.push(src[i]);
                    }
                } else if src[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < src.len() {
            match src[i + 1] {
                '/' => {
                    while i < src.len() && src[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                '*' => {
                    i += 2;
                    while i + 1 < src.len() && !(src[i] == '*' && src[i + 1] == '/') {
                        i += 1;
                    }
                    i = (i + 2).min(src.len());
                    out.push(' ');
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn find_from(chars: &[char], needle: &str, from: usize) -> Option<usize> {
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() || from + n.len() > chars.len() {
        return None;
    }
    (from..=chars.len() - n.len()).find(|&i| chars[i..i + n.len()] == n[..])
}

fn skip_ws(chars: &[char], i: &mut usize) {
    while *i < chars.len() && chars[*i].is_whitespace() {
        *i += 1;
    }
}

/// 跳过一对配平的分隔符（泛型里的 `SearchResult<HoardProject>` 有嵌套）。
/// 返回 false 表示没有找到闭合，这次匹配作废。
fn skip_balanced(chars: &[char], i: &mut usize, open: char, close: char) -> bool {
    let mut depth = 0usize;
    while *i < chars.len() {
        match chars[*i] {
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    *i += 1;
                    return true;
                }
            }
            _ => {}
        }
        *i += 1;
    }
    false
}

fn diff(a: &BTreeSet<String>, b: &BTreeSet<String>) -> Vec<String> {
    a.difference(b).cloned().collect()
}

#[test]
fn every_defined_command_is_registered() {
    let defined = defined_commands();
    let registered = registered_commands();
    assert!(!defined.is_empty(), "no commands parsed — 解析规则失效了？");
    assert!(
        diff(&defined, &registered).is_empty(),
        "defined but NOT registered in generate_handler! (点了会 command not found): {:?}",
        diff(&defined, &registered)
    );
    assert!(
        diff(&registered, &defined).is_empty(),
        "registered but NOT defined in commands.rs (编译期不会报，运行时才炸): {:?}",
        diff(&registered, &defined)
    );
}

#[test]
fn every_command_has_a_frontend_caller() {
    let defined = defined_commands();
    let invoked = invoked_commands();
    assert!(
        !invoked.is_empty(),
        "no invoke() calls parsed — 解析规则失效了？"
    );

    let unused = diff(&defined, &invoked);
    assert!(
        unused.is_empty(),
        "命令没有任何前端调用方，等于没有被验证过。接上 UI，或先别加：{unused:?}"
    );
    let unknown = diff(&invoked, &defined);
    assert!(unknown.is_empty(), "前端调用了不存在的命令：{unknown:?}");
}

#[test]
fn events_are_declared_on_both_sides() {
    // 事件名同样是两边手写的字符串；写错一个字符的表现是"日志页永远空白"
    let rust = read("src/commands.rs");
    let ts = read("../src/lib/ipc.ts");
    for event in [
        "compass://progress",
        "compass://game-log",
        "compass://game-exit",
        "compass://copy-progress",
    ] {
        assert!(rust.contains(event), "Rust 侧不再推送 {event}");
        assert!(ts.contains(event), "前端没有监听 {event}");
    }
}

#[test]
fn the_ts_parser_only_matches_real_calls() {
    let src = r#"
import { invoke } from "@tauri-apps/api/core";
/** invoke 的拒绝值是我们序列化的 { code, message } */
export const ipc = {
  a: () => invoke<A>("cmd_a"),
  b: (x: string) =>
    invoke<SearchResult<HoardProject>>("cmd_b", { x }),
  c: () => invoke<
    Multi
  >(
    "cmd_c",
  ),
  // d: () => invoke<void>("cmd_commented_out"),
  /* e: () => invoke<void>("cmd_block_commented") */
  url: "http://example.com/invoke",
  invoked: () => invoke<void>("cmd_f"),
};
"#;
    let got: Vec<String> = parse_invoked(src).into_iter().collect();
    assert_eq!(
        got,
        vec!["cmd_a", "cmd_b", "cmd_c", "cmd_f"],
        "import/注释/字符串里的 invoke 都不算调用"
    );
}

#[test]
fn the_rust_parsers_find_real_entries() {
    // 解析规则失效时集合会变空，那会让上面两道断言全部"通过"（空集没有差集）。
    // 所以先确认解析器确实在解析东西。
    assert!(defined_commands().len() >= 15, "{:?}", defined_commands());
    assert!(
        registered_commands().len() >= 15,
        "{:?}",
        registered_commands()
    );
    assert!(invoked_commands().len() >= 15, "{:?}", invoked_commands());
    assert!(
        defined_commands().contains("instance_launch"),
        "instance_launch 应当被解析出来"
    );
}
