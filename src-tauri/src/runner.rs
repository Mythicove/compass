//! 把「实例 + 账号 + 全局配置」拼成一条可执行的命令行。
//!
//! 这一层刻意**不碰网络、不 spawn 进程**：Java 版本要求、authlib-injector 注入、
//! 各条路径的拼装全在这里，因此可以离线单测；下载与进程管理留给调用方（commands）。

use std::path::PathBuf;

use crate::account::Account;
use crate::authlibinjector;
use crate::config::Config;
use crate::error::Error;
use crate::install::split_libraries;
use crate::instance::{self, Instance};
use crate::java;
use crate::launch::{self, LaunchOptions, LaunchPlan};
use crate::meta::MetaClient;
use crate::net::Downloader;
use crate::paths::Paths;
use crate::version::{OsInfo, VersionJson};

/// 需要 Java 的哪个大版本，以及是否必须**正好**是它。
///
/// 老版本（1.16 及以下，`jre-legacy`）必须用 Java 8：新 JDK 移除了它们依赖的
/// 内部 API，LWJGL2 会直接崩。1.17 起是"至少这个版本"，往上兼容。
pub fn java_requirement(v: &VersionJson) -> (u32, bool) {
    match v.java_version.as_ref().map(|j| j.major_version) {
        Some(major) if major <= 8 => (8, true),
        Some(major) => (major, false),
        // 没有 javaVersion 字段的都是远古版本（1.16 之前才有这种 JSON）
        None => (8, true),
    }
}

pub struct LaunchInput<'a> {
    pub instance: &'a Instance,
    pub account: &'a Account,
    pub version: &'a VersionJson,
    pub os: &'a OsInfo,
    pub paths: &'a Paths,
    /// 全局默认内存上限（MiB）；实例自己的 `memory_mb` 优先。
    pub default_memory_mb: u32,
    pub api_url: &'a str,
    pub java: PathBuf,
    /// authlib-injector 的 jar（None = 不注入，游戏能启动但认不到我们的皮肤站）。
    pub authlib_jar: Option<PathBuf>,
    /// 预取的 mirror meta（base64），省掉启动期一次网络请求。
    pub prefetched_meta: Option<String>,
}

/// 组装启动计划。
pub fn build(input: &LaunchInput) -> Result<LaunchPlan, Error> {
    let game_dir = instance::game_dir(input.paths, &input.instance.slug)?;
    let natives_dir = instance::natives_dir(input.paths, &input.instance.slug)?;
    let asset_index_id = input
        .version
        .asset_index_id()
        .ok_or_else(|| {
            Error::Invalid(format!(
                "version {} has no asset index (安装不完整？)",
                input.version.id
            ))
        })?
        .to_string();

    // 先查 client jar 再查库：两者都属于"版本没装完"，但 jar 是更根本的那个，
    // 报它更符合用户的直觉（"这版本没装"而不是"某个 lwjgl 分片不见了"）。
    let jar_version = input.version.jar_version();
    let client_jar = input
        .paths
        .versions()
        .join(jar_version)
        .join(format!("{jar_version}.jar"));
    if !client_jar.is_file() {
        return Err(Error::Config(format!(
            "client jar missing: {} (先安装该版本)",
            client_jar.display()
        )));
    }

    let (classpath, _) = split_libraries(input.version, input.os, input.paths);
    if classpath.is_empty() {
        return Err(Error::Config(format!(
            "version {} resolved to an empty classpath (安装不完整？)",
            input.version.id
        )));
    }
    // 逐个确认库文件真的在磁盘上：少一个 jar 的话，JVM 抛的是
    // ClassNotFoundException/NoClassDefFoundError，用户看不出"是版本没装完"。
    // 85 次 stat 约 1ms，换一句能读懂的错误值得。
    if let Some(missing) = classpath.iter().find(|p| !p.is_file()) {
        return Err(Error::Config(format!(
            "library missing: {} (版本 {} 未装完整，请重新安装)",
            missing.display(),
            input.version.id
        )));
    }

    // 日志配置文件：只有真的在磁盘上才传，否则 JVM 会因为找不到文件启动失败
    let logging_config = input
        .version
        .logging
        .as_ref()
        .and_then(|l| l.client.as_ref())
        .map(|c| {
            input
                .paths
                .versions()
                .join(&input.version.id)
                .join(&c.file.id)
        })
        .filter(|p| p.is_file());

    let mut extra_jvm_args = Vec::new();
    if let Some(jar) = &input.authlib_jar {
        extra_jvm_args.push(authlibinjector::agent_arg(jar, input.api_url));
        if let Some(meta) = &input.prefetched_meta {
            extra_jvm_args.push(authlibinjector::prefetched_arg(meta));
        }
    }

    let opts = LaunchOptions {
        game_dir,
        natives_dir,
        assets_root: input.paths.assets(),
        asset_index_id,
        classpath,
        libraries_root: input.paths.libraries(),
        client_jar,
        java: input.java.clone(),
        // 实例覆盖优先，其次全局默认
        memory_mb: input.instance.memory_mb.unwrap_or(input.default_memory_mb),
        // 分辨率/直连服务器/演示模式随 P2 的实例选项落地
        resolution: None,
        server: None,
        demo: false,
        player_name: input.account.player_name.clone(),
        player_uuid: input.account.player_uuid.clone(),
        access_token: input.account.access_token.clone(),
        // ${version_name} 用版本 id：它是游戏里 F3 与"版本"显示的真值，
        // 塞实例名会让崩溃报告对不上任何已知版本
        version_name: input.version.id.clone(),
        version_type: if input.version.kind.is_empty() {
            "release".to_string()
        } else {
            input.version.kind.clone()
        },
        logging_config,
        extra_jvm_args,
    };
    launch::build_plan(input.version, input.os, &opts)
}

/// natives 解压清单（每次启动前重建）。
pub fn natives_to_extract(input: &LaunchInput) -> Vec<(PathBuf, Vec<String>)> {
    split_libraries(input.version, input.os, input.paths).1
}

/// 一次启动所需的全部产物：命令行 + 用哪个 Java。
pub struct PreparedLaunch {
    pub plan: LaunchPlan,
    pub java_version: u32,
    /// 是否成功注入了 authlib-injector（没注入也能启动，只是认不到我们的皮肤站）。
    pub injected: bool,
}

/// 启动编排的**唯一实现**：读已装版本 → 选 Java → 备 authlib-injector → 拼命令行 → 解压 natives。
///
/// 命令层与冒烟测试都走这里，避免"测试里重写一遍顺序"——那种测试只能证明它自己是对的。
pub async fn prepare_launch(
    paths: &Paths,
    http: &reqwest::Client,
    cfg: &Config,
    inst: &Instance,
    account: &Account,
) -> Result<PreparedLaunch, Error> {
    let version = read_installed_version(paths, &inst.version_id)?;
    let os = OsInfo::current();

    // Java：实例指定优先，否则按版本要求探测（老版本必须正好是 Java 8）
    let (major, exact) = java_requirement(&version);
    let runtime = match &inst.java_path {
        Some(p) => java::probe(p)
            .await
            .ok_or_else(|| Error::Config(format!("{} is not a usable java", p.display())))?,
        None => java::find(major, exact, Some(&paths.java()))
            .await
            .ok_or_else(|| {
                Error::Config(format!(
                    "no Java {major} runtime found (安装 JDK {major} 或在实例里指定路径)"
                ))
            })?,
    };

    // authlib-injector：拿不到就不注入（游戏仍能启动，只是没有我们的皮肤），
    // 但不能因为下载失败就整个启动不了
    let dl = Downloader::new(
        http.clone(),
        cfg.download.concurrency,
        cfg.download.mirror.clone(),
    );
    let injector = authlibinjector::ensure(paths, &dl).await.ok();
    let prefetched = match &injector {
        Some(_) => {
            let meta = MetaClient::new(http.clone(), cfg.download.mirror.clone());
            authlibinjector::prefetch_meta(&meta, &cfg.api_url).await
        }
        None => None,
    };

    let input = LaunchInput {
        instance: inst,
        account,
        version: &version,
        os: &os,
        paths,
        default_memory_mb: cfg.defaults.memory_mb,
        api_url: &cfg.api_url,
        java: runtime.path.clone(),
        authlib_jar: injector,
        prefetched_meta: prefetched.clone(),
    };
    let plan = build(&input)?;
    let natives = natives_to_extract(&input);
    let natives_dir = instance::natives_dir(paths, &inst.slug)?;
    launch::extract_natives(&natives, &natives_dir)?;
    // 以命令行里真的有 -javaagent 为准（而不是"我们下载到了 jar"）：
    // 注入没生效却报告 injected=true，会让"皮肤不出"的排查从一开始就跑偏
    let injected = plan.args.iter().any(|a| a.starts_with("-javaagent:"));
    Ok(PreparedLaunch {
        plan,
        java_version: runtime.version,
        injected,
    })
}

/// 安装期落盘的合并版本 JSON 路径（"装没装好"与"读它"共用同一条规则）。
pub fn installed_version_path(paths: &Paths, version_id: &str) -> PathBuf {
    paths
        .versions()
        .join(version_id)
        .join(format!("{version_id}.json"))
}

/// 读安装期落盘的合并版本 JSON。
pub fn read_installed_version(paths: &Paths, version_id: &str) -> Result<VersionJson, Error> {
    let file = installed_version_path(paths, version_id);
    let text = std::fs::read_to_string(&file).map_err(|_| {
        Error::Invalid(format!(
            "version {version_id} is not installed (先安装该版本)"
        ))
    })?;
    VersionJson::parse(&text).map_err(|e| Error::Config(format!("{}: {e}", file.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{Arch, OsName};

    fn parse(json: &str) -> VersionJson {
        VersionJson::parse(json).unwrap()
    }

    /// 把一个版本"装"到临时数据目录里：写版本 JSON、造 client jar 与库文件。
    /// 这样 build() 的存在性检查才有东西可查，测试也不需要联网。
    fn fake_install(paths: &Paths, v: &VersionJson) {
        let dir = paths.versions().join(&v.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.json", v.id)),
            serde_json::to_vec(v).unwrap(),
        )
        .unwrap();
        let jar_version = v.jar_version().to_string();
        let jar_dir = paths.versions().join(&jar_version);
        std::fs::create_dir_all(&jar_dir).unwrap();
        std::fs::write(jar_dir.join(format!("{jar_version}.jar")), b"fake jar").unwrap();
        let os = OsInfo::new(OsName::Windows, "10.0", Arch::X64);
        for lib in crate::version::resolve_libraries(v, &os, &crate::version::Features::new()) {
            let p = paths.libraries().join(&lib.path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"fake lib").unwrap();
        }
        if let Some(logging) = v.logging.as_ref().and_then(|l| l.client.as_ref()) {
            std::fs::write(dir.join(&logging.file.id), b"<Configuration/>").unwrap();
        }
    }

    fn input<'a>(
        paths: &'a Paths,
        version: &'a VersionJson,
        instance: &'a Instance,
        account: &'a Account,
        os: &'a OsInfo,
    ) -> LaunchInput<'a> {
        LaunchInput {
            instance,
            account,
            version,
            os,
            paths,
            default_memory_mb: 4096,
            api_url: "https://mc.example.com",
            java: PathBuf::from(r"C:\jdk\bin\java.exe"),
            authlib_jar: None,
            prefetched_meta: None,
        }
    }

    fn win() -> OsInfo {
        OsInfo::new(OsName::Windows, "10.0", Arch::X64)
    }

    fn sample_account() -> Account {
        Account {
            player_name: "Alice".into(),
            player_uuid: "7e01ed00f3453fbcb4437bac57a28a65".into(),
            access_token: "tok".into(),
            client_token: "ct".into(),
            api_url: "https://mc.example.com".into(),
            signed_in_at: 0,
        }
    }

    #[test]
    fn java_requirement_is_exact_for_legacy() {
        assert_eq!(
            java_requirement(&parse(include_str!("../tests/fixtures/1.21.4.json"))),
            (21, false)
        );
        assert_eq!(
            java_requirement(&parse(include_str!("../tests/fixtures/1.20.1.json"))),
            (17, false)
        );
        assert_eq!(
            java_requirement(&parse(include_str!("../tests/fixtures/1.16.5.json"))),
            (8, true)
        );
        assert_eq!(
            java_requirement(&parse(include_str!("../tests/fixtures/1.12.2.json"))),
            (8, true)
        );
        // 没有 javaVersion 字段的远古 JSON 也按 Java 8 处理
        let mut ancient = parse(include_str!("../tests/fixtures/1.7.10.json"));
        ancient.java_version = None;
        assert_eq!(java_requirement(&ancient), (8, true));
    }

    #[test]
    fn builds_a_launchable_plan_from_an_installed_version() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("data"));
        paths.ensure().unwrap();
        let version = parse(include_str!("../tests/fixtures/1.21.4.json"));
        fake_install(&paths, &version);
        let instance = Instance {
            slug: "main".into(),
            name: "Main".into(),
            version_id: version.id.clone(),
            memory_mb: Some(6144),
            ..Instance::default()
        };
        instance::save(&paths, &instance).unwrap();
        let account = sample_account();

        let plan = build(&input(&paths, &version, &instance, &account, &win())).unwrap();
        assert_eq!(plan.program, PathBuf::from(r"C:\jdk\bin\java.exe"));
        // 内存用实例覆盖值，不是全局默认
        assert!(
            plan.args.contains(&"-Xmx6144M".to_string()),
            "{:?}",
            plan.args
        );
        // 身份来自账号（玩家名 + 独立密码换来的令牌）
        assert!(plan.args.iter().any(|a| a == "Alice"));
        assert!(plan.args.iter().any(|a| a == "tok"));
        // 游戏目录是实例自己的 .minecraft，natives 在实例目录下
        assert!(
            plan.work_dir.ends_with("main/.minecraft")
                || plan.work_dir.to_string_lossy().contains("main")
        );
        assert!(plan.args.iter().any(|a| a.contains("natives")));
        // 日志配置文件存在时才传
        assert!(
            plan.args
                .iter()
                .any(|a| a.starts_with("-Dlog4j.configurationFile="))
        );
        // client jar 在 classpath 末尾
        let cp_idx = plan.args.iter().position(|a| a == "-cp").unwrap();
        assert!(plan.args[cp_idx + 1].ends_with("1.21.4.jar"));
        assert!(!plan.args.iter().any(|a| a.contains("${")));
    }

    #[test]
    fn injects_authlib_injector_before_the_main_class() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("data"));
        paths.ensure().unwrap();
        let version = parse(include_str!("../tests/fixtures/1.21.4.json"));
        fake_install(&paths, &version);
        let instance = Instance {
            slug: "main".into(),
            version_id: version.id.clone(),
            ..Instance::default()
        };
        instance::save(&paths, &instance).unwrap();
        let account = sample_account();

        let os = win();
        let mut in_ = input(&paths, &version, &instance, &account, &os);
        in_.authlib_jar = Some(paths.tools().join("authlib-injector-1.2.8.jar"));
        in_.prefetched_meta = Some("eyJhIjoxfQ==".into());
        let plan = build(&in_).unwrap();

        let main_idx = plan
            .args
            .iter()
            .position(|a| a == "net.minecraft.client.main.Main")
            .unwrap();
        let agent_idx = plan
            .args
            .iter()
            .position(|a| a.starts_with("-javaagent:"))
            .unwrap();
        assert!(agent_idx < main_idx);
        // api root 不带尾斜杠，且 injector 自己拼 /authserver 等路径
        assert!(
            plan.args[agent_idx].ends_with("=https://mc.example.com"),
            "{}",
            plan.args[agent_idx]
        );
        assert!(
            plan.args
                .iter()
                .any(|a| a == "-Dauthlibinjector.yggdrasil.prefetched=eyJhIjoxfQ==")
        );
    }

    #[test]
    fn refuses_to_build_when_the_install_is_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("data"));
        paths.ensure().unwrap();
        let version = parse(include_str!("../tests/fixtures/1.21.4.json"));
        let instance = Instance {
            slug: "main".into(),
            version_id: version.id.clone(),
            ..Instance::default()
        };
        instance::save(&paths, &instance).unwrap();
        let account = sample_account();
        // 没装过：client jar 不存在
        let err = build(&input(&paths, &version, &instance, &account, &win())).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "{err:?}");
        assert!(err.to_string().contains("client jar missing"), "{err}");

        // 只补上 client jar、库里一个文件都没有 → 必须在拼命令行时就报出缺哪个 jar，
        // 而不是让 JVM 抛一句 ClassNotFoundException 让用户猜
        let jar_dir = paths.versions().join(version.jar_version());
        std::fs::create_dir_all(&jar_dir).unwrap();
        std::fs::write(jar_dir.join("1.21.4.jar"), b"fake").unwrap();
        let err2 = build(&input(&paths, &version, &instance, &account, &win())).unwrap_err();
        assert!(err2.to_string().contains("library missing"), "{err2}");
        assert!(err2.to_string().contains(".jar"), "{err2}");
    }

    #[test]
    fn natives_list_matches_the_version_generation() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("data"));
        paths.ensure().unwrap();
        let modern = parse(include_str!("../tests/fixtures/1.21.4.json"));
        let legacy = parse(include_str!("../tests/fixtures/1.12.2.json"));
        let instance = Instance {
            slug: "main".into(),
            ..Instance::default()
        };
        let account = sample_account();
        assert!(
            natives_to_extract(&input(&paths, &modern, &instance, &account, &win())).is_empty()
        );
        assert!(
            !natives_to_extract(&input(&paths, &legacy, &instance, &account, &win())).is_empty()
        );
    }
}
