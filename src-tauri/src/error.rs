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
    /// 后端返回非 2xx。`code` 取 body 里的稳定错误码（如 mirror 的 `binding_required`），
    /// 取不到时按状态码给一个通用码，前端至少能落到状态码文案。
    #[error("api {status}: {detail}")]
    Api {
        status: u16,
        code: String,
        detail: String,
    },
    #[error("invalid url: {0}")]
    Url(String),
    #[error("invalid input: {0}")]
    Invalid(String),
}

impl Error {
    /// 前端可 switch 的稳定错误码。后端给了稳定码（如 `binding_required`）就原样透传，
    /// 否则按错误类别给一个通用码。错误是罕见路径，这里的一次分配不值得优化。
    pub fn code(&self) -> String {
        match self {
            Error::Config(_) => "config",
            Error::Io(_) => "io",
            Error::Http(_) => "network",
            Error::Api { code, .. } if !code.is_empty() => code.as_str(),
            Error::Api { .. } => "api",
            Error::Url(_) | Error::Invalid(_) => "invalid",
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
