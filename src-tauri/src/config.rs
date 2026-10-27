//! 全局设置（`compass.json`）。
//!
//! 三个刻意的选择：
//! - **缺字段用默认值补**（`#[serde(default)]`）：升级后老配置文件照样能读，不需要版本号与迁移代码。
//! - **读完必 sanitize**：手改文件、旧版本残留、UI 传进来的越界值都在这一处收敛，
//!   后续代码可以假定 `concurrency >= 1`、URL 合法，不必处处防御。
//! - **原子写**（临时文件 + rename）：写到一半断电不该让用户下次启动丢掉全部设置。

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::paths::Paths;

/// Mirror（皮肤站 + Yggdrasil + OIDC）的地址。n 合一部署时 Hoard 与它同域。
const DEFAULT_API_URL: &str = "http://localhost:8080";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Mirror 基址（不带尾斜杠）。
    pub api_url: String,
    /// Hoard 基址；默认与 api_url 相同（n 合一部署经同一网关按路径分流）。
    pub hoard_url: String,
    /// UI 语言；空串 = 跟随系统。
    pub language: String,
    /// `system` | `dark` | `light`。
    pub theme: String,
    pub download: Download,
    pub defaults: Defaults,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Download {
    /// 并发下载数。
    pub concurrency: usize,
    /// 资源/库镜像源前缀；空串 = 官方源。
    pub mirror: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Defaults {
    /// 新实例的默认最大内存（MiB）。
    pub memory_mb: u32,
}

impl Default for Download {
    fn default() -> Self {
        Self {
            concurrency: 32,
            mirror: String::new(),
        }
    }
}

impl Default for Defaults {
    fn default() -> Self {
        Self { memory_mb: 4096 }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_string(),
            hoard_url: DEFAULT_API_URL.to_string(),
            language: String::new(),
            theme: "system".to_string(),
            download: Download::default(),
            defaults: Defaults::default(),
        }
    }
}

impl Config {
    /// 把外部输入收敛到合法区间；非法 URL 直接报错（静默改成默认值会让用户以为设置生效了）。
    pub fn sanitize(mut self) -> Result<Self, Error> {
        self.api_url = normalize_base_url(&self.api_url)?;
        self.hoard_url = normalize_base_url(&self.hoard_url)?;
        self.download.concurrency = self.download.concurrency.clamp(1, 64);
        self.defaults.memory_mb = self.defaults.memory_mb.clamp(512, 65_536);
        if !matches!(self.theme.as_str(), "system" | "dark" | "light") {
            return Err(Error::Invalid(format!("unknown theme: {:?}", self.theme)));
        }
        Ok(self)
    }
}

/// 校验并规范化基址：必须是 http/https 的绝对 URL，去掉尾斜杠（拼接时统一加 `/`）。
fn normalize_base_url(raw: &str) -> Result<String, Error> {
    let trimmed = raw.trim().trim_end_matches('/');
    let url = url::Url::parse(trimmed).map_err(|e| Error::Url(format!("{trimmed:?}: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(Error::Url(format!("not an http(s) base url: {trimmed:?}")));
    }
    Ok(trimmed.to_string())
}

/// 读取设置。文件不存在 = 首次启动，返回默认值（不写盘，留给用户第一次保存）。
/// 文件损坏返回错误而不是静默重置：静默重置会让用户以为设置"莫名消失"。
pub fn load(paths: &Paths) -> Result<Config, Error> {
    load_from(paths.config_file().as_path())
}

fn load_from(file: &Path) -> Result<Config, Error> {
    match fs::read_to_string(file) {
        Ok(text) => {
            let cfg: Config = serde_json::from_str(&text)
                .map_err(|e| Error::Config(format!("{}: {e}", file.display())))?;
            cfg.sanitize()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// 原子写：先写同目录临时文件再 rename（同目录才保证 rename 是原子的）。
pub fn save(paths: &Paths, cfg: &Config) -> Result<(), Error> {
    save_to(paths.config_file().as_path(), cfg)
}

fn save_to(file: &Path, cfg: &Config) -> Result<(), Error> {
    let cfg = cfg.clone().sanitize()?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(&cfg).map_err(|e| Error::Config(e.to_string()))?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        assert_eq!(load(&paths).unwrap(), Config::default());
        assert!(!paths.config_file().exists());
    }

    #[test]
    fn unknown_fields_and_missing_fields_survive() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        // 只有 api_url：其余字段取默认（升级后老配置照样能读）；多余字段被忽略
        fs::write(
            paths.config_file(),
            r#"{"api_url":"https://mirror.example.com/","future_field":1}"#,
        )
        .unwrap();
        let cfg = load(&paths).unwrap();
        assert_eq!(cfg.api_url, "https://mirror.example.com");
        assert_eq!(cfg.hoard_url, DEFAULT_API_URL);
        assert_eq!(cfg.download.concurrency, 32);
    }

    #[test]
    fn sanitize_clamps_and_rejects() {
        let cfg = Config {
            download: Download {
                concurrency: 0,
                mirror: String::new(),
            },
            defaults: Defaults { memory_mb: 1 },
            ..Config::default()
        };
        let fixed = cfg.sanitize().unwrap();
        assert_eq!(fixed.download.concurrency, 1);
        assert_eq!(fixed.defaults.memory_mb, 512);

        assert!(
            Config {
                api_url: "not a url".into(),
                ..Config::default()
            }
            .sanitize()
            .is_err()
        );
        assert!(
            Config {
                api_url: "ftp://mirror.example.com".into(),
                ..Config::default()
            }
            .sanitize()
            .is_err()
        );
        assert!(
            Config {
                theme: "neon".into(),
                ..Config::default()
            }
            .sanitize()
            .is_err()
        );
    }

    #[test]
    fn trailing_slash_is_normalized() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let cfg = Config {
            api_url: "https://mirror.example.com///".into(),
            ..Config::default()
        };
        save(&paths, &cfg).unwrap();
        assert_eq!(load(&paths).unwrap().api_url, "https://mirror.example.com");
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        paths.ensure().unwrap();
        save(&paths, &Config::default()).unwrap();
        assert!(paths.config_file().exists());
        assert!(!paths.config_file().with_extension("json.tmp").exists());
    }

    #[test]
    fn corrupt_file_is_an_error_not_a_silent_reset() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        fs::write(paths.config_file(), "{ not json").unwrap();
        let err = load(&paths).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "got {err:?}");
    }
}
