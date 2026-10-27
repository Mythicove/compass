//! authlib-injector：让游戏客户端认我们的 Yggdrasil 认证服务（皮肤、离线服务器进服）。
//!
//! 版本与校验和**写死在代码里**，不跟 latest：这是个以 `-javaagent` 注入游戏 JVM 的组件，
//! 等于把"能在玩家进程里执行任意代码"的东西交给远端决定。升级 = 改这三个常量 + 跑冒烟测试
//! （与 [ADR-0008](https://docs.gov.skin/architecture/adr-0008-rsa-key-management/) 对签名密钥的态度一致）。
//!
//! 除了 sha1（下载器的通用校验），这里额外做一次 **sha256**：它是唯一会在用户机器上
//! 执行代码的下载物，值得比"内容对不对"更强的保证。

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::meta::MetaClient;
use crate::net::{Downloader, FileTask, sha256_of_file};
use crate::paths::Paths;

pub const VERSION: &str = "1.2.8";
pub const DOWNLOAD_URL: &str = "https://github.com/yushijinhun/authlib-injector/releases/download/v1.2.8/authlib-injector-1.2.8.jar";
pub const SHA1: &str = "0e0e66d8a4f91a26f33b9c09f5cdffce4a11f0b8";
pub const SHA256: &str = "9c7f4343e6c82034958ffb48c14a2cb0c85928be7283103ce17da00c6d5a7b10";
pub const SIZE: u64 = 349_681;

fn file_name() -> String {
    format!("authlib-injector-{VERSION}.jar")
}

pub fn jar_path(paths: &Paths) -> PathBuf {
    paths.tools().join(file_name())
}

/// 确保 jar 就位并通过 sha256 校验，返回其路径。
///
/// 已存在但校验不过时会删掉重下一次（磁盘损坏/半截文件都可能），再不过就报错——
/// 宁可启动失败，也不要注入一个内容不明的 javaagent。
pub async fn ensure(paths: &Paths, dl: &Downloader) -> Result<PathBuf, Error> {
    let dest = jar_path(paths);
    for _ in 0..2 {
        if dest.is_file() && sha256_of_file(&dest).await.ok().as_deref() == Some(SHA256) {
            return Ok(dest);
        }
        let _ = tokio::fs::remove_file(&dest).await;
        let summary = dl
            .download_all(
                vec![FileTask {
                    url: DOWNLOAD_URL.to_string(),
                    dest: dest.clone(),
                    sha1: SHA1.to_string(),
                    size: SIZE,
                }],
                None,
            )
            .await;
        if !summary.failed.is_empty() {
            return Err(Error::Config(format!(
                "failed to download authlib-injector {VERSION}: {}",
                summary.failed.join("; ")
            )));
        }
    }
    let actual = sha256_of_file(&dest).await?;
    if actual != SHA256 {
        let _ = tokio::fs::remove_file(&dest).await;
        return Err(Error::Checksum(format!(
            "authlib-injector {VERSION} sha256 mismatch: want {SHA256} got {actual}"
        )));
    }
    Ok(dest)
}

/// `-javaagent:<jar>=<api root>`。api root 是 Mirror 的基址（authlib-injector 会自己拼
/// `/authserver`、`/sessionserver` 等路径），所以传站点根而不是某个具体端点。
pub fn agent_arg(jar: &Path, api_root: &str) -> String {
    format!(
        "-javaagent:{}={}",
        jar.display(),
        api_root.trim_end_matches('/')
    )
}

/// 预取 Mirror 的 `GET /` meta 并 base64，交给 `-Dauthlibinjector.yggdrasil.prefetched`。
/// 省掉启动期的一次网络请求（慢网/离线下更稳，也是 HMCL 的做法）。
/// 失败不致命：不传这个参数时 authlib-injector 会自己去请求。
pub async fn prefetch_meta(meta: &MetaClient, api_root: &str) -> Option<String> {
    let base = api_root.trim_end_matches('/');
    let text = meta.text(base).await.ok()?;
    // 只接受看起来像 meta 的响应：把 HTML 错误页塞给 injector 会让它报一堆莫名错误
    if !text.contains("signaturePublickey") {
        return None;
    }
    Some(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        text.as_bytes(),
    ))
}

pub fn prefetched_arg(encoded: &str) -> String {
    format!("-Dauthlibinjector.yggdrasil.prefetched={encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_artifact_is_fully_specified() {
        // 三个常量缺一不可：少一个就等于把校验让给了远端
        assert_eq!(VERSION, "1.2.8");
        assert!(DOWNLOAD_URL.contains(VERSION), "{DOWNLOAD_URL}");
        assert_eq!(SHA1.len(), 40);
        assert_eq!(SHA256.len(), 64);
        assert_eq!(file_name(), "authlib-injector-1.2.8.jar");
    }

    #[test]
    fn jar_lives_in_the_shared_tools_cache() {
        let paths = Paths::new("/data");
        assert_eq!(
            jar_path(&paths),
            PathBuf::from("/data/cache/tools/authlib-injector-1.2.8.jar")
        );
    }

    #[test]
    fn agent_arg_points_at_the_site_root() {
        let jar = Path::new("/data/cache/tools/authlib-injector-1.2.8.jar");
        assert_eq!(
            agent_arg(jar, "https://mc.example.com/"),
            "-javaagent:/data/cache/tools/authlib-injector-1.2.8.jar=https://mc.example.com"
        );
        // api root 不是某个端点：injector 自己拼 /authserver 等路径
        assert!(!agent_arg(jar, "https://mc.example.com").contains("/authserver"));
    }

    #[test]
    fn prefetched_arg_is_base64_of_the_meta() {
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            br#"{"skinDomains":["mc.example.com"]}"#,
        );
        let arg = prefetched_arg(&encoded);
        assert!(arg.starts_with("-Dauthlibinjector.yggdrasil.prefetched="));
        let b64 = arg.split_once('=').unwrap().1;
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64).unwrap();
        assert_eq!(decoded, br#"{"skinDomains":["mc.example.com"]}"#);
    }

    #[tokio::test]
    async fn prefetch_rejects_non_meta_responses() {
        // 用本地端口 1（必然连不上）验"失败不致命"：返回 None 而不是错误
        let meta = MetaClient::new(reqwest::Client::new(), "");
        assert!(prefetch_meta(&meta, "http://127.0.0.1:1").await.is_none());
    }
}
