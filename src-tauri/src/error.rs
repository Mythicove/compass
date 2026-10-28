//! 应用错误。命令层统一序列化成 `{ code, message }`：前端按 `code` 本地化，
//! `message` 只用于日志与"复制错误"，不直接渲染给用户（与 web 的 errors.* 键同一套思路）。

use serde::Serialize;
use serde::ser::SerializeStruct;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// jar/zip 读取失败（natives 解压、P3 的整合包安装）。
    #[error("archive: {0}")]
    Archive(#[from] zip::result::ZipError),
    /// 后端返回非 2xx。`code` 取 body 里的稳定错误码（如 mirror 的 `binding_required`），
    /// 取不到时按状态码给一个通用码，前端至少能落到状态码文案。
    #[error("api {status}: {detail}")]
    Api {
        status: u16,
        code: String,
        detail: String,
    },
    /// 下载内容的 sha1/大小与来源给的不一致：文件已丢弃，绝不落到目标路径。
    #[error("checksum mismatch: {0}")]
    Checksum(String),
    #[error("invalid url: {0}")]
    Url(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    /// 请求的东西不存在（实例、内容文件、已装版本）。与 `Invalid` 分开：
    /// 前者 UI 该说"没找到"，后者说"参数不对"，混在一起时用户看不出该改输入还是该先安装。
    #[error("not found: {0}")]
    NotFound(String),
    /// 需要登录（启动游戏要用玩家名 + 令牌）。
    #[error("not signed in")]
    NotSignedIn,
}

impl Error {
    /// 前端可 switch 的稳定错误码。后端给了稳定码（如 `binding_required`）就原样透传，
    /// 否则按错误类别给一个通用码。错误是罕见路径，这里的一次分配不值得优化。
    pub fn code(&self) -> String {
        match self {
            Error::Config(_) => "config",
            Error::Io(_) => "io",
            Error::Http(_) => "network",
            Error::Archive(_) => "archive",
            Error::Api { code, .. } if !code.is_empty() => code.as_str(),
            Error::Api { .. } => "api",
            Error::Checksum(_) => "checksum",
            Error::Url(_) | Error::Invalid(_) => "invalid",
            Error::NotFound(_) => "not_found",
            Error::NotSignedIn => "auth",
        }
        .to_string()
    }
}

impl Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Error", 2)?;
        s.serialize_field("code", &self.code())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}
