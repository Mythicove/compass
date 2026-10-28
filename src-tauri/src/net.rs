//! 下载器：并发、重试、sha1 校验、断点续传、镜像重写。**全站唯一一处下载实现**
//! （计划 P0 决策 4）：版本 JSON、资源对象、库文件、日志配置、JRE 都走它。
//!
//! 两条不可让步的规则：
//! - **校验失败的文件一律丢弃**：下到 `<dest>.part`，校验通过才 rename 成正式文件。
//!   半截 jar 会让游戏以"极难诊断"的方式崩溃，比直接失败糟糕得多。
//! - **一批文件不因个别失败而中止**：能下的都下完，最后统一返回失败清单，
//!   让 UI 一次说清缺了什么（而不是修一个再跑一次）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;
use serde::Serialize;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::error::Error;

/// 重试次数与退避基数。镜像抖动很常见，但重试不是无限的：卡住的文件要能报出来。
const ATTEMPTS: u32 = 3;
const BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);

/// 官方源主机 → 它在镜像上的路径前缀。不在表里的主机（maven.fabricmc.net、
/// maven.neoforged.net、GitHub release 等）**一律不改写**：镜像并不代管它们，
/// 改写了只会把一个能用的 URL 变成 404。
///
/// 前缀不是可有可无的细节，两个都是实测出来的：
/// - BMCLAPI 把 `libraries.minecraft.net` 挂在 `/maven/` 下；
/// - 官方资源 URL 是 `<host>/<aa>/<hash>`（**没有** `objects/`），而 BMCLAPI 把它挂在
///   `/objects/` 下——所以资源这一条必须补前缀，否则配了镜像反而全 404。
///
/// 少了这两条映射，配镜像的用户会丢掉数量最多的两类文件（库与资源对象）。
fn mirror_prefix(host: &str) -> Option<&'static str> {
    Some(match host {
        "libraries.minecraft.net" => "/maven",
        "resources.download.minecraft.net" => "/objects",
        "piston-meta.mojang.com"
        | "piston-data.mojang.com"
        | "launchermeta.mojang.com"
        | "launcher.mojang.com" => "",
        _ => return None,
    })
}

/// 把官方源 URL 改写到镜像；镜像为空或主机不在白名单时原样返回。
/// 这里假定镜像是 **BMCLAPI 兼容布局**（目前唯一广泛使用的镜像约定）；
/// 布局不同的镜像应当留空不用，而不是让我们去猜。
pub fn rewrite_mirror(url: &str, mirror: &str) -> String {
    let mirror = mirror.trim().trim_end_matches('/');
    if mirror.is_empty() {
        return url.to_string();
    }
    let Ok(parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    let Some(host) = parsed.host_str() else {
        return url.to_string();
    };
    let Some(prefix) = mirror_prefix(host) else {
        return url.to_string();
    };
    format!("{mirror}{prefix}{}", parsed.path())
}

/// 一个文件要依次尝试的 URL：镜像优先，**镜像失败退回官方源**。
///
/// 这不是多余的保险：镜像会出现"部分路径 403/404、其余正常"的状态（实测 BMCLAPI 的
/// /maven 在部分网络下就是这样），此时官方源反而是通的。反过来在官方源被墙的网络里，
/// 回退只会多一次快速失败，代价可接受。
///
/// 非官方源（Fabric/NeoForge 的 Maven、GitHub release）既不改写也不回退——它们只有一个来源。
pub fn candidate_urls(url: &str, mirror: &str) -> Vec<String> {
    let rewritten = rewrite_mirror(url, mirror);
    if rewritten == url {
        vec![url.to_string()]
    } else {
        vec![rewritten, url.to_string()]
    }
}

#[derive(Debug, Clone)]
pub struct FileTask {
    pub url: String,
    pub dest: PathBuf,
    /// 空串表示来源没给校验和（Fabric 的 intermediary/loader 就没有）：跳过校验而不是当空哈希。
    pub sha1: String,
    pub size: u64,
}

impl FileTask {
    /// `.part` 落点：下载与校验都发生在这里，成功后才 rename。
    pub fn part_path(&self) -> PathBuf {
        part_path(&self.dest)
    }
}

/// `.part` 落点规则（只有一处）：调用方在下载失败后要清掉半成品，
/// 而"半成品叫什么名字"不该由调用方自己拼一遍。
pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// 单个文件的下载进度（UI 用它画总进度与当前文件明细）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileProgress {
    pub url: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub downloaded: usize,
    pub skipped: usize,
    pub bytes: u64,
    pub failed: Vec<String>,
}

/// 进度用 channel 而不是回调闭包：Tauri 那边转成事件，测试这边直接收。
pub type ProgressSink = tokio::sync::mpsc::UnboundedSender<FileProgress>;

#[derive(Clone)]
pub struct Downloader {
    http: reqwest::Client,
    concurrency: usize,
    mirror: Arc<str>,
}

impl Downloader {
    pub fn new(http: reqwest::Client, concurrency: usize, mirror: impl Into<String>) -> Self {
        Self {
            http,
            concurrency: concurrency.clamp(1, 64),
            mirror: Arc::from(mirror.into()),
        }
    }

    /// 已经就位（存在、大小对、校验和通过）就不用再下。
    async fn is_complete(task: &FileTask) -> bool {
        let Ok(meta) = tokio::fs::metadata(&task.dest).await else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        if task.size > 0 && meta.len() != task.size {
            return false;
        }
        if task.sha1.is_empty() {
            // 没有校验和可比，只能以大小为准；两者都没有时，0 字节文件不可能是完整内容，
            // 视为未就位强制重下（否则一次失败留下的空壳会永远冒充"已下载"）
            return task.size > 0 || meta.len() > 0;
        }
        sha1_of_file(&task.dest).await.ok().as_deref() == Some(task.sha1.as_str())
    }

    /// 下载单个文件：续传 + 重试 + 校验 + 原子落位。返回本次实际写入的字节数（0 = 本来就有）。
    async fn download_one(
        &self,
        task: &FileTask,
        progress: &Option<ProgressSink>,
    ) -> Result<u64, Error> {
        if Self::is_complete(task).await {
            return Ok(0);
        }
        if let Some(parent) = task.dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let part = task.part_path();

        let mut last_err = Error::Config("download failed".into());
        // 镜像优先，失败退回官方源（见 candidate_urls 的理由）
        for url in candidate_urls(&task.url, &self.mirror) {
            for attempt in 0..ATTEMPTS {
                if attempt > 0 {
                    tokio::time::sleep(BACKOFF * attempt).await;
                }
                match self.attempt(&url, &part, task, progress).await {
                    Ok(bytes) => {
                        // 校验不过就删掉重来：半截/损坏文件绝不能被当成"已下载"
                        if let Err(e) = verify(&part, task).await {
                            let _ = tokio::fs::remove_file(&part).await;
                            last_err = e;
                            continue;
                        }
                        tokio::fs::rename(&part, &task.dest).await?;
                        return Ok(bytes);
                    }
                    // 网络类错误保留 .part 以便下次续传
                    Err(e) => last_err = e,
                }
            }
        }
        Err(last_err)
    }

    async fn attempt(
        &self,
        url: &str,
        part: &Path,
        task: &FileTask,
        progress: &Option<ProgressSink>,
    ) -> Result<u64, Error> {
        // 续传：已有 .part 就带 Range；服务端不支持（回 200）则从头写
        let have = tokio::fs::metadata(part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let mut req = self.http.get(url);
        let mut resume_from = 0u64;
        if have > 0 && (task.size == 0 || have < task.size) {
            req = req.header("Range", format!("bytes={have}-"));
            resume_from = have;
        }
        let res = req.send().await?;
        let (written_start, append) = match res.status().as_u16() {
            206 => (have, true),
            200 => (0, false),
            // 416 = Range 超出范围：文件其实已经完整，交给校验判定
            416 if have > 0 => return Ok(0),
            other => {
                let body = res.text().await.unwrap_or_default();
                return Err(Error::Api {
                    status: other,
                    code: String::new(),
                    detail: body.chars().take(200).collect(),
                });
            }
        };
        let total = if task.size > 0 {
            task.size
        } else {
            resume_from + res.content_length().unwrap_or(0)
        };

        let mut file = if append {
            tokio::fs::OpenOptions::new()
                .append(true)
                .open(part)
                .await?
        } else {
            tokio::fs::File::create(part).await?
        };
        let mut written = written_start;
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            written += chunk.len() as u64;
            if let Some(tx) = progress {
                // 进度丢了不该让下载失败（订阅方可能已经关掉）
                let _ = tx.send(FileProgress {
                    url: task.url.clone(),
                    done_bytes: written,
                    total_bytes: total,
                });
            }
        }
        file.flush().await?;
        // 本次实际传输的字节：从 written_start 起算。服务端对 Range 回 200（不支持续传）
        // 时 written_start=0 而 resume_from=have，减 resume_from 会下溢。
        Ok(written - written_start)
    }

    /// 并发下载一批文件，返回汇总。个别失败不影响其余文件。
    pub async fn download_all(
        &self,
        tasks: Vec<FileTask>,
        progress: Option<ProgressSink>,
    ) -> Summary {
        let summary = Arc::new(Mutex::new(Summary::default()));
        let mut stream = futures_util::stream::iter(tasks.into_iter().map(|task| {
            let this = self.clone();
            let progress = progress.clone();
            let summary = Arc::clone(&summary);
            async move {
                match this.download_one(&task, &progress).await {
                    Ok(0) => summary.lock().await.skipped += 1,
                    Ok(bytes) => {
                        let mut s = summary.lock().await;
                        s.downloaded += 1;
                        s.bytes += bytes;
                    }
                    Err(e) => {
                        // 报**实际请求的 URL**：配了镜像时只显示原始域名的话，
                        // 用户根本没法判断是源站坏了还是镜像坏了。
                        let effective = rewrite_mirror(&task.url, &this.mirror);
                        let label = if effective == task.url {
                            task.url.clone()
                        } else {
                            format!("{} (via {effective})", task.url)
                        };
                        summary.lock().await.failed.push(format!("{label}: {e}"));
                    }
                }
            }
        }))
        .buffer_unordered(self.concurrency);
        while stream.next().await.is_some() {}
        summary.lock().await.clone()
    }
}

/// 把失败清单压成一句人能读的话：最多列 5 条，其余只报数量。
/// 一次装版本可能有几千个文件，把全部失败塞进一个错误串只会让 UI 和日志没法看。
pub fn format_failures(failed: &[String]) -> String {
    const SHOW: usize = 5;
    let shown: Vec<&str> = failed.iter().take(SHOW).map(String::as_str).collect();
    let mut msg = shown.join("; ");
    if failed.len() > SHOW {
        msg.push_str(&format!("; … 另 {} 个", failed.len() - SHOW));
    }
    msg
}

/// 校验 sha1 与大小。没有校验和时只比大小（来源没给就无从校验，记录在案即可）。
async fn verify(path: &Path, task: &FileTask) -> Result<(), Error> {
    if !task.sha1.is_empty() {
        let actual = sha1_of_file(path).await?;
        if actual != task.sha1 {
            return Err(Error::Checksum(format!(
                "{}: want {} got {actual}",
                task.url, task.sha1
            )));
        }
    }
    if task.size > 0 {
        let len = tokio::fs::metadata(path).await?.len();
        if len != task.size {
            return Err(Error::Checksum(format!(
                "{}: want {} bytes got {len}",
                task.url, task.size
            )));
        }
    }
    Ok(())
}

/// 流式算 sha1（不把整个文件读进内存：client.jar 有几十 MB）。
pub async fn sha1_of_file(path: &Path) -> Result<String, Error> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// 流式算 sha256。只给 authlib-injector 这类"会在用户机器上执行代码"的下载物用：
/// 它们值得比通用 sha1 更强的保证。
pub async fn sha256_of_file(path: &Path) -> Result<String, Error> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::{TcpListener, TcpStream};

    fn sha1_hex(bytes: &[u8]) -> String {
        let mut h = Sha1::new();
        h.update(bytes);
        hex::encode(h.finalize())
    }

    #[test]
    fn mirror_rewrites_only_official_hosts() {
        let m = "https://bmclapi2.bangbang93.com";
        // libraries 在 BMCLAPI 上挂在 /maven/ 下（不是路径 1:1）：
        // 少了这条映射，配了镜像的用户的库文件会全部 404
        assert_eq!(
            rewrite_mirror("https://libraries.minecraft.net/org/lwjgl/x.jar", m),
            "https://bmclapi2.bangbang93.com/maven/org/lwjgl/x.jar"
        );
        // 资源对象与 piston-* 才是路径 1:1
        assert_eq!(
            rewrite_mirror("https://piston-data.mojang.com/v1/objects/ab/client.jar", m),
            "https://bmclapi2.bangbang93.com/v1/objects/ab/client.jar"
        );
        // 资源对象：官方 URL 没有 objects/ 这一层，BMCLAPI 有 ⇒ 必须补前缀
        assert_eq!(
            rewrite_mirror("https://resources.download.minecraft.net/ab/abcd", m),
            "https://bmclapi2.bangbang93.com/objects/ab/abcd"
        );
        // 非官方源不改写：镜像不代管它们，改了只会 404
        assert_eq!(
            rewrite_mirror("https://maven.fabricmc.net/net/fabricmc/x.jar", m),
            "https://maven.fabricmc.net/net/fabricmc/x.jar"
        );
        // 未配置镜像 / 镜像带尾斜杠 / 坏 URL
        assert_eq!(
            rewrite_mirror("https://piston-data.mojang.com/x", ""),
            "https://piston-data.mojang.com/x"
        );
        assert_eq!(
            rewrite_mirror("https://piston-data.mojang.com/x", "https://m.example/"),
            "https://m.example/x"
        );
        assert_eq!(rewrite_mirror("not a url", m), "not a url");
    }

    #[test]
    fn failures_are_truncated_to_something_readable() {
        let many: Vec<String> = (0..12).map(|i| format!("file{i}: 404")).collect();
        let msg = format_failures(&many);
        assert!(msg.contains("file0: 404"));
        assert!(msg.contains("file4: 404"));
        assert!(!msg.contains("file5"), "只列前 5 条：{msg}");
        assert!(msg.contains("另 7 个"), "{msg}");
        assert_eq!(format_failures(&[]), "");
        assert_eq!(format_failures(&["only-one".to_string()]), "only-one");
    }

    #[test]
    fn mirror_is_tried_first_then_the_official_source() {
        let m = "https://bmclapi2.bangbang93.com";
        // 官方源：镜像在前，官方在后（镜像部分失效时才有救）
        assert_eq!(
            candidate_urls("https://libraries.minecraft.net/org/lwjgl/x.jar", m),
            vec![
                "https://bmclapi2.bangbang93.com/maven/org/lwjgl/x.jar".to_string(),
                "https://libraries.minecraft.net/org/lwjgl/x.jar".to_string()
            ]
        );
        // 没配镜像：只有一个候选，不做无谓的第二次尝试
        assert_eq!(
            candidate_urls("https://libraries.minecraft.net/x.jar", ""),
            vec!["https://libraries.minecraft.net/x.jar".to_string()]
        );
        // 非官方源：不改写也就不回退（它只有一个来源）
        assert_eq!(
            candidate_urls("https://maven.fabricmc.net/net/fabricmc/x.jar", m),
            vec!["https://maven.fabricmc.net/net/fabricmc/x.jar".to_string()]
        );
    }

    #[test]
    fn part_path_is_dest_plus_suffix() {
        let t = FileTask {
            url: "u".into(),
            dest: PathBuf::from("/cache/lib/a.jar"),
            sha1: String::new(),
            size: 0,
        };
        assert_eq!(t.part_path(), PathBuf::from("/cache/lib/a.jar.part"));
    }

    #[tokio::test]
    async fn sha1_of_file_matches_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.bin");
        tokio::fs::write(&p, b"abc").await.unwrap();
        assert_eq!(
            sha1_of_file(&p).await.unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    /// 最小 HTTP 服务：支持 Range（回 206），并可让前 N 次请求直接断连（验重试）。
    struct TestServer {
        addr: String,
    }

    impl TestServer {
        async fn start(body: Vec<u8>, fail_times: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let fails = Arc::new(AtomicUsize::new(fail_times));
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let body = body.clone();
                    let fails = Arc::clone(&fails);
                    tokio::spawn(async move {
                        let _ = handle(stream, body, fails).await;
                    });
                }
            });
            Self { addr }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{path}", self.addr)
        }
    }

    async fn handle(
        mut stream: TcpStream,
        body: Vec<u8>,
        fails: Arc<AtomicUsize>,
    ) -> std::io::Result<()> {
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await?;
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        if fails.load(Ordering::SeqCst) > 0 {
            fails.fetch_sub(1, Ordering::SeqCst);
            // 直接断开：模拟镜像抖动，触发重试
            return Ok(());
        }
        let mut from = 0usize;
        for line in head.lines() {
            if let Some(rest) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                from = rest.trim_end_matches('-').parse().unwrap_or(0);
            }
        }
        let payload = body[from.min(body.len())..].to_vec();
        let status = if from > 0 {
            "206 Partial Content"
        } else {
            "200 OK"
        };
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        stream.write_all(resp.as_bytes()).await?;
        stream.write_all(&payload).await?;
        stream.flush().await
    }

    fn downloader() -> Downloader {
        Downloader::new(reqwest::Client::new(), 4, "")
    }

    #[tokio::test]
    async fn downloads_verifies_and_skips_when_complete() {
        let body = b"hello minecraft".to_vec();
        let srv = TestServer::start(body.clone(), 0).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("lib/a.jar");
        let task = FileTask {
            url: srv.url("/a.jar"),
            dest: dest.clone(),
            sha1: sha1_hex(&body),
            size: body.len() as u64,
        };

        let summary = downloader().download_all(vec![task.clone()], None).await;
        assert_eq!(summary.failed, Vec::<String>::new());
        assert_eq!(summary.downloaded, 1);
        assert_eq!(summary.bytes, body.len() as u64);
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), body);
        assert!(!task.part_path().exists(), ".part must be renamed away");

        // 再跑一次：已就位，不重复下载
        let again = downloader().download_all(vec![task], None).await;
        assert_eq!(again.downloaded, 0);
        assert_eq!(again.skipped, 1);
        assert_eq!(again.bytes, 0);
    }

    #[tokio::test]
    async fn resumes_from_partial_file() {
        let body = b"0123456789abcdef".to_vec();
        let srv = TestServer::start(body.clone(), 0).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("b.jar");
        // 预置半截 .part：应带 Range 续传，而不是从头再来
        tokio::fs::write(dest.with_extension("jar.part"), &body[..6])
            .await
            .unwrap();

        let task = FileTask {
            url: srv.url("/b.jar"),
            dest: dest.clone(),
            sha1: sha1_hex(&body),
            size: body.len() as u64,
        };
        let summary = downloader().download_all(vec![task], None).await;
        assert_eq!(summary.failed, Vec::<String>::new());
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), body);
        assert_eq!(summary.bytes, (body.len() - 6) as u64, "只应补下缺的部分");
    }

    #[tokio::test]
    async fn checksum_mismatch_is_discarded_not_kept() {
        let srv = TestServer::start(b"real content".to_vec(), 0).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("c.jar");
        let task = FileTask {
            url: srv.url("/c.jar"),
            dest: dest.clone(),
            sha1: "0".repeat(40),
            size: 0,
        };
        let summary = downloader().download_all(vec![task], None).await;
        assert_eq!(summary.failed.len(), 1, "{summary:?}");
        // 关键：损坏文件既不能落到 dest，也不能留个 .part 冒充已下载
        assert!(!dest.exists());
        assert!(!dest.with_extension("jar.part").exists());
    }

    #[tokio::test]
    async fn retries_transient_disconnects() {
        let body = b"flaky mirror".to_vec();
        // 前两次请求直接断连，第三次成功
        let srv = TestServer::start(body.clone(), 2).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("d.jar");
        let task = FileTask {
            url: srv.url("/d.jar"),
            dest: dest.clone(),
            sha1: sha1_hex(&body),
            size: body.len() as u64,
        };
        let summary = downloader().download_all(vec![task], None).await;
        assert_eq!(summary.failed, Vec::<String>::new(), "{summary:?}");
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), body);
    }

    #[tokio::test]
    async fn one_failure_does_not_stop_the_batch() {
        let srv = TestServer::start(b"ok".to_vec(), 0).await;
        let dir = tempfile::tempdir().unwrap();
        let good = FileTask {
            url: srv.url("/good.jar"),
            dest: dir.path().join("good.jar"),
            sha1: String::new(),
            size: 0,
        };
        let bad = FileTask {
            // 端口 1 上没人听：连接必然失败
            url: "http://127.0.0.1:1/nope.jar".into(),
            dest: dir.path().join("bad.jar"),
            sha1: String::new(),
            size: 0,
        };
        let summary = downloader()
            .download_all(vec![good.clone(), bad], None)
            .await;
        assert_eq!(summary.failed.len(), 1, "{summary:?}");
        assert_eq!(summary.downloaded, 1);
        assert!(good.dest.exists());
    }

    #[tokio::test]
    async fn progress_events_are_emitted() {
        let body = vec![7u8; 4096];
        let srv = TestServer::start(body.clone(), 0).await;
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let task = FileTask {
            url: srv.url("/p.jar"),
            dest: dir.path().join("p.jar"),
            sha1: String::new(),
            size: body.len() as u64,
        };
        let summary = downloader().download_all(vec![task], Some(tx)).await;
        assert_eq!(summary.failed, Vec::<String>::new());
        let mut last = 0;
        let mut events = 0;
        while let Ok(p) = rx.try_recv() {
            assert_eq!(p.total_bytes, body.len() as u64);
            assert!(p.done_bytes >= last);
            last = p.done_bytes;
            events += 1;
        }
        assert!(events > 0, "should have emitted progress");
        assert_eq!(
            last,
            body.len() as u64,
            "last progress should be the full size"
        );
    }
}
