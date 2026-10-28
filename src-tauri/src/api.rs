//! 后端 HTTP 客户端。**所有网络请求都在 Rust 侧**（计划文档 P0 决策 2）：
//! webview 不再直接 fetch 后端，于是桌面端既不依赖 Mirror 的 CORS 配置，
//! 也不需要 CSP 的 `connect-src` 放行远端——CSP 因此可以收得很紧。
//!
//! 只暴露**具名方法**，不提供"请求任意 URL"的通用命令：
//! 通用代理等于把本机变成一个开放代理，也会让 CSP 收紧失去意义。

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// 连接/整体超时。启动器会在弱网与镜像不可达的环境下运行，无超时的请求会一直挂着。
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
}

/// 全站共用的 HTTP 客户端（连接池复用）：ApiClient、MetaClient 与下载器都用它。
pub fn http_client() -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("MythicCompass/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

impl ApiClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    // ---- Mirror / Yggdrasil ----

    /// 启动器登录：**玩家名 + 该玩家的独立密码**（ADR-0012）。
    /// 网站主密码在这个端点上无效，邮箱也不能作用户名。
    pub async fn authenticate(
        &self,
        base: &str,
        player_name: &str,
        password: &str,
    ) -> Result<AuthenticateResponse, Error> {
        let body = serde_json::json!({ "username": player_name, "password": password });
        self.post_json(&format!("{base}/authserver/authenticate"), body)
            .await
    }

    /// 校验令牌是否仍然有效（启动时判断"要不要重新登录"）。
    pub async fn validate(
        &self,
        base: &str,
        access_token: &str,
        client_token: &str,
    ) -> Result<(), Error> {
        let body = serde_json::json!({ "accessToken": access_token, "clientToken": client_token });
        let res = self
            .http
            .post(format!("{base}/authserver/validate"))
            .json(&body)
            .send()
            .await?;
        match res.status().as_u16() {
            204 => Ok(()),
            status => Err(api_error(status, &res.text().await.unwrap_or_default())),
        }
    }

    /// 作废令牌（登出用）。走 invalidate 而不是 signout：signout 要玩家名 + 独立密码，
    /// 而启动器不存密码（也不该存），invalidate 只需要我们手上的 accessToken + clientToken。
    pub async fn invalidate(
        &self,
        base: &str,
        access_token: &str,
        client_token: &str,
    ) -> Result<(), Error> {
        let body = serde_json::json!({ "accessToken": access_token, "clientToken": client_token });
        let res = self
            .http
            .post(format!("{base}/authserver/invalidate"))
            .json(&body)
            .send()
            .await?;
        match res.status().as_u16() {
            204 => Ok(()),
            status => Err(api_error(status, &res.text().await.unwrap_or_default())),
        }
    }

    /// 取皮肤 PNG。无皮肤时 Mirror 返回 404，此处映射为 `None`（前端回落内置默认皮肤）。
    pub async fn skin_png(&self, base: &str, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let res = self
            .http
            .get(join_path(base, &["skin", name])?)
            .send()
            .await?;
        match res.status().as_u16() {
            200 => Ok(Some(res.bytes().await?.to_vec())),
            404 => Ok(None),
            status => Err(api_error(status, &res.text().await.unwrap_or_default())),
        }
    }

    // ---- Hoard ----

    pub async fn hoard_search(
        &self,
        base: &str,
        q: &str,
        offset: i64,
        limit: i64,
    ) -> Result<SearchResponse<Project>, Error> {
        let res = self
            .http
            .get(format!("{base}/api/mods"))
            .query(&[
                ("q", q),
                ("offset", &offset.to_string()),
                ("limit", &limit.to_string()),
            ])
            .send()
            .await?;
        self.json_or_error(res).await
    }

    pub async fn hoard_versions(
        &self,
        base: &str,
        project_id: &str,
    ) -> Result<Vec<Version>, Error> {
        let res = self
            .http
            .get(join_path(base, &["v2", "project", project_id, "version"])?)
            .send()
            .await?;
        self.json_or_error(res).await
    }

    // ---- 内部 ----

    async fn post_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        body: serde_json::Value,
    ) -> Result<T, Error> {
        let res = self.http.post(url).json(&body).send().await?;
        self.json_or_error(res).await
    }

    async fn json_or_error<T: for<'de> Deserialize<'de>>(
        &self,
        res: reqwest::Response,
    ) -> Result<T, Error> {
        let status = res.status().as_u16();
        let text = res.text().await?;
        if !(200..300).contains(&status) {
            return Err(api_error(status, &text));
        }
        serde_json::from_str(&text).map_err(|e| Error::Config(format!("decode response: {e}")))
    }

    /// GET 一个 JSON 端点。状态码处理与解码只在这一处，
    /// 免得每个新数据源（loader meta、整合包源）各写一遍而漏掉非 2xx。
    pub async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, Error> {
        let res = self.http.get(url).send().await?;
        self.json_or_error(res).await
    }
}

/// 把动态值按 URL 路径段百分号编码后拼进地址。name/project_id 来自 UI 与后端数据，
/// 直接内插等于让它们改写请求目标（`../`、`?`、`#`、`/` 都有 URL 语义）。
pub(crate) fn join_path(base: &str, segments: &[&str]) -> Result<String, Error> {
    let mut url = url::Url::parse(base).map_err(|e| Error::Url(format!("{base:?}: {e}")))?;
    {
        let mut paths = url
            .path_segments_mut()
            .map_err(|_| Error::Url(format!("not a usable base url: {base:?}")))?;
        for seg in segments {
            paths.push(seg);
        }
    }
    Ok(url.to_string())
}

/// 把非 2xx 变成 `Error::Api`，并尽量抽出**稳定错误码**。
/// Mirror 有两种错误形状：账号端点 `{"error": ...}`（本地化文案或稳定码），
/// Yggdrasil 端点 `{"error": ..., "errorMessage": ...}`（稳定码在 errorMessage 里）。
pub(crate) fn api_error(status: u16, body: &str) -> Error {
    let code = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            ["error", "errorMessage"]
                .into_iter()
                .filter_map(|k| v.get(k).and_then(|x| x.as_str()))
                .find(|s| is_stable_code(s))
                .map(str::to_string)
        })
        .unwrap_or_default();
    Error::Api {
        status,
        code,
        detail: body.chars().take(300).collect(),
    }
}

/// 稳定错误码的形状：全小写 ASCII + 下划线（如 `binding_required`）。
/// 本地化文案必然含空格/大写/标点，不会被误判成码。
fn is_stable_code(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// 把二进制转成 `data:` URL，交给 webview 里的 skinview3d 当纹理。
/// 这样 CSP 的 `img-src` 只需放行 `data:`，不必放行任意外站图片。
pub fn png_data_url(bytes: &[u8]) -> String {
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

// ---- 线上形状（只声明用得到的字段，其余忽略）----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub premium: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticateResponse {
    pub access_token: String,
    pub client_token: String,
    #[serde(default)]
    pub available_profiles: Vec<Player>,
    pub selected_profile: Option<Player>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse<T> {
    pub hits: Vec<T>,
    pub offset: i64,
    pub limit: i64,
    pub total_hits: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub project_type: String,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub downloads: i64,
    #[serde(default)]
    pub published: String,
}

/// Hoard 的 version。必填字段以 `api/specs/hoard.openapi.yaml` 的 `required` 为准
/// （`id` / `project_id` / `version_number`）——比规范更严会让一个**合法**响应
/// 把整个版本列表解码失败，UI 上表现为"搜索到了但点不开"。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub id: String,
    pub project_id: String,
    pub version_number: String,
    #[serde(default)]
    pub version_type: String,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub files: Vec<File>,
}

/// Hoard 的 version file。规范里没有 `required`，所以全部字段都容忍缺省；
/// 真正用到它的地方（`hoard_install`）会校验：空 filename 过不了
/// `content::validate_file_name`，空 url 过不了 `Url::parse`，各自报错而不是静默下载个空文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct File {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub size: i64,
    /// 规范里是 `sha1` + `sha512`（没有 sha256）。缺 sha1 时下载器跳过校验——
    /// 那是"无从校验"，不是"校验通过"。
    #[serde(default)]
    pub hashes: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_codes_are_recognized_and_localized_text_is_not() {
        assert!(is_stable_code("binding_required"));
        assert!(is_stable_code("password_login_disabled"));
        assert!(!is_stable_code("ForbiddenOperationException"));
        assert!(!is_stable_code(
            "Invalid credentials. Invalid username or password."
        ));
        assert!(!is_stable_code("本站已关闭账号密码登录"));
        assert!(!is_stable_code(""));
    }

    #[test]
    fn yggdrasil_error_carries_the_code_from_error_message() {
        let err = api_error(
            403,
            r#"{"error":"ForbiddenOperationException","errorMessage":"binding_required"}"#,
        );
        match err {
            Error::Api { status, code, .. } => {
                assert_eq!(status, 403);
                assert_eq!(code, "binding_required");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn account_error_carries_the_code_from_error() {
        match api_error(403, r#"{"error":"password_login_disabled"}"#) {
            Error::Api { code, .. } => assert_eq!(code, "password_login_disabled"),
            other => panic!("unexpected {other:?}"),
        }
        // 本地化文案不是码：code 留空，前端落到状态码文案
        match api_error(400, r#"{"error":"验证码校验失败"}"#) {
            Error::Api { code, .. } => assert!(code.is_empty()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn path_segments_are_escaped_not_interpolated() {
        assert_eq!(
            join_path("https://mirror.example.com", &["skin", "Alice"]).unwrap(),
            "https://mirror.example.com/skin/Alice"
        );
        // `../` 与查询符必须被编码成字面量，不能改写路径或带上 query
        let evil = join_path("https://mirror.example.com", &["skin", "../../admin?x=1"]).unwrap();
        assert!(evil.starts_with("https://mirror.example.com/skin/"));
        assert!(!evil.contains("/admin"), "{evil}");
        assert!(evil.contains("%2F") || evil.contains("%3F"), "{evil}");
        assert!(join_path("not a url", &["x"]).is_err());
    }

    #[test]
    fn data_url_is_png_typed() {
        let url = png_data_url(&[0x89, b'P', b'N', b'G']);
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn authenticate_response_parses_the_wire_shape() {
        let json = r#"{
            "accessToken":"a","clientToken":"c",
            "availableProfiles":[{"id":"u","name":"Alice"}],
            "selectedProfile":{"id":"u","name":"Alice","premium":false}
        }"#;
        let r: AuthenticateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.selected_profile.unwrap().name, "Alice");
        assert_eq!(r.available_profiles.len(), 1);
    }

    /// Hoard 的响应形状。真值在三处，改任何一处都要回来看这条测试：
    /// - `mythic-hoard/internal/hoard/store.go`：`Project` / `Version` / `File` 的 json tag
    /// - `mythic-hoard/internal/hoard/service.go`：`SearchResult{hits,offset,limit,total_hits}`
    /// - `api/specs/hoard.openapi.yaml`：对外契约（`File.hashes` 是 **sha1 + sha512**，没有 sha256）
    ///
    /// 样本刻意包含"字段缺省"的情形：Go 侧 `loaders`/`game_versions` 带 `omitempty`，
    /// 空数组时整个键不出现——我们这边必须有 `#[serde(default)]`，否则一次搜索就整体解码失败。
    #[test]
    fn hoard_wire_shapes_parse_including_omitted_arrays() {
        let search = r#"{
            "hits": [
                {
                    "id": "p1", "slug": "sodium", "title": "Sodium",
                    "description": "A modern rendering engine", "project_type": "mod",
                    "downloads": 12345, "published": "2024-01-02T03:04:05Z",
                    "loaders": ["fabric"], "game_versions": ["1.21.4"]
                },
                {
                    "id": "p2", "slug": "minimal", "title": "Minimal",
                    "project_type": "mod", "downloads": 0, "published": ""
                }
            ],
            "offset": 0, "limit": 50, "total_hits": 2
        }"#;
        let res: SearchResponse<Project> = serde_json::from_str(search).unwrap();
        assert_eq!(res.total_hits, 2);
        assert_eq!(res.hits[0].loaders, vec!["fabric".to_string()]);
        // omitempty 的键整个不存在时必须是空数组，而不是解码失败
        assert!(res.hits[1].loaders.is_empty(), "{:?}", res.hits[1].loaders);
        assert!(res.hits[1].game_versions.is_empty());
        assert_eq!(res.hits[1].description, "", "description 也不是必填");

        let versions = r#"[
            {
                "id": "v1", "project_id": "p1", "name": "Sodium 0.6.13",
                "version_number": "0.6.13", "version_type": "release",
                "loaders": ["fabric"], "game_versions": ["1.21.4"],
                "date_published": "2024-12-03T10:12:57Z",
                "files": [
                    {
                        "url": "https://cdn.example.com/files/ab/cd/sodium.jar",
                        "filename": "sodium-fabric-0.6.13+mc1.21.4.jar",
                        "primary": true, "size": 1306799,
                        "hashes": {
                            "sha1": "8f6f0c1e5d4b3a29187f6e5d4c3b2a1908f7e6d5",
                            "sha512": "abcd"
                        }
                    },
                    {
                        "url": "https://cdn.example.com/files/ab/cd/sodium-sources.jar",
                        "filename": "sodium-fabric-0.6.13-sources.jar",
                        "primary": false, "size": 1, "hashes": {}
                    }
                ]
            },
            { "id": "v2", "project_id": "p1", "version_number": "0.6.12", "files": [] }
        ]"#;
        let list: Vec<Version> = serde_json::from_str(versions).unwrap();
        assert_eq!(list.len(), 2);
        // 版本可以只有规范里的三个必填字段（id/project_id/version_number）
        assert_eq!(list[1].version_type, "");
        assert!(list[1].loaders.is_empty());

        let primary = list[0]
            .files
            .iter()
            .find(|f| f.primary)
            .expect("应当能挑出 primary 文件");
        assert_eq!(primary.filename, "sodium-fabric-0.6.13+mc1.21.4.jar");
        assert_eq!(primary.size, 1306799);
        // 安装时校验的就是这个 sha1（Hoard 上传时一定会算，见 storeFile）；
        // 拿不到 sha1 就是"无从校验"，不能当成"校验通过"
        assert_eq!(
            primary.hashes.get("sha1").map(String::as_str),
            Some("8f6f0c1e5d4b3a29187f6e5d4c3b2a1908f7e6d5")
        );
        assert!(
            !list[0].files[1].hashes.contains_key("sha1"),
            "空 hashes 里没有 sha1，不是空串——两者在下载器里含义不同"
        );
    }
}
