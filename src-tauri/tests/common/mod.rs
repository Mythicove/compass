//! 契约门共用的解析工具。
//!
//! 这个 crate 有两道"两侧手写、没人比对"的契约门（见 `ipc_contract.rs` 与
//! `shape_contract.rs`）。它们都要读仓库里的源文件、剥 TypeScript 注释，
//! 所以把这部分放一起：解析规则只有一份，两道门对"什么算注释"的理解不会分岔。
//!
//! 放在 `tests/common/` 下是为了不被当成一个独立的测试二进制编译。
//!
//! 这个模块被编进每个用到它的测试二进制，而每道门只用其中一部分 helper，
//! 所以整体允许 dead_code——否则 `cargo clippy -D warnings` 会因为
//! "shape_contract 里没用到 find_from" 而红。
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

/// `src-tauri` 目录（`CARGO_MANIFEST_DIR`）。前端文件在它的上一层。
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 读一个仓库内文件，路径相对于 `src-tauri`（所以前端文件写 `../src/...`）。
pub fn read(rel: &str) -> String {
    let p = root().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// 去掉 TypeScript 的 `//` 与 `/* */` 注释。字符串与模板字面量里的 `//` 不算注释
/// （否则一个 URL 会把后面的代码整段吃掉）。
pub fn strip_ts_comments(text: &str) -> Vec<char> {
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

/// 从 `from` 起找 `needle` 的第一次出现。
pub fn find_from(chars: &[char], needle: &str, from: usize) -> Option<usize> {
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() || from + n.len() > chars.len() {
        return None;
    }
    (from..=chars.len() - n.len()).find(|&i| chars[i..i + n.len()] == n[..])
}

pub fn skip_ws(chars: &[char], i: &mut usize) {
    while *i < chars.len() && chars[*i].is_whitespace() {
        *i += 1;
    }
}

/// 跳过一对配平的分隔符（泛型里的 `SearchResult<HoardProject>` 有嵌套）。
/// 返回 false 表示没有找到闭合。
pub fn skip_balanced(chars: &[char], i: &mut usize, open: char, close: char) -> bool {
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

/// snake_case → camelCase（serde 的 `rename_all = "camelCase"` 做的事）。
pub fn to_camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = false;
    for c in snake.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_matches_serde_rename_all() {
        assert_eq!(to_camel("memory_mb"), "memoryMb");
        assert_eq!(to_camel("version_id"), "versionId");
        assert_eq!(to_camel("slug"), "slug");
        assert_eq!(to_camel("last_launched_at"), "lastLaunchedAt");
        assert_eq!(to_camel("extra_jvm_args"), "extraJvmArgs");
    }

    #[test]
    fn comment_stripping_keeps_strings_intact() {
        let src = r#"
const url = "http://example.com/a//b"; // 行注释
/* 块注释 const x = 1; */
const t = `模板 // 也不是注释`;
"#;
        let out: String = strip_ts_comments(src).into_iter().collect();
        assert!(out.contains("http://example.com/a//b"), "{out}");
        assert!(out.contains("模板 // 也不是注释"), "{out}");
        assert!(!out.contains("行注释"), "{out}");
        assert!(!out.contains("块注释"), "{out}");
    }

    #[test]
    fn balanced_skip_handles_nested_generics() {
        let chars: Vec<char> = "<SearchResult<HoardProject>>(\"x\")".chars().collect();
        let mut i = 0;
        assert!(skip_balanced(&chars, &mut i, '<', '>'));
        assert_eq!(&chars[i..].iter().collect::<String>(), "(\"x\")");
    }
}
