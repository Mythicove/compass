// 与 Rust 命令层一一对应的类型化封装。
//
// 前端不再直接 fetch 后端：所有 HTTP 都在 Rust 侧（src-tauri/src/api.rs），
// 于是桌面端不依赖 Mirror 的 CORS 配置，CSP 也能收紧到不放行任何远端 connect/img。
// 令牌同理不进 webview——authLogin 返回的是不含令牌的 AccountView。
import { invoke } from "@tauri-apps/api/core";

export interface DownloadConfig {
  concurrency: number;
  mirror: string;
}

export interface Defaults {
  memoryMb: number;
}

export interface Config {
  apiUrl: string;
  hoardUrl: string;
  /** 空串 = 跟随系统 */
  language: string;
  theme: "system" | "dark" | "light";
  download: DownloadConfig;
  defaults: Defaults;
}

export interface PathsInfo {
  root: string;
  configFile: string;
  accountsFile: string;
  cache: string;
  assets: string;
  libraries: string;
  versions: string;
  java: string;
  tools: string;
  instances: string;
}

export interface AccountView {
  playerName: string;
  playerUuid: string;
  apiUrl: string;
  signedInAt: number;
}

export interface Player {
  id: string;
  name: string;
  premium?: boolean;
}

export interface HoardProject {
  id: string;
  slug: string;
  title: string;
  description: string;
  project_type: string;
  loaders?: string[];
  game_versions?: string[];
  downloads: number;
  published: string;
}

export interface HoardFile {
  url: string;
  filename: string;
  primary: boolean;
  size: number;
  hashes?: Record<string, string>;
}

export interface HoardVersion {
  id: string;
  project_id: string;
  version_number: string;
  version_type: string;
  loaders?: string[];
  game_versions?: string[];
  files: HoardFile[];
}

export interface SearchResult<T> {
  hits: T[];
  offset: number;
  limit: number;
  total_hits: number;
}

/** Rust 侧 `Error` 序列化后的形状：`code` 用于本地化，`message` 只用于日志。 */
export interface IpcErrorBody {
  code: string;
  message: string;
}

export class IpcError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "IpcError";
  }
}

/** invoke 的拒绝值是我们序列化的 { code, message }；其它异常归为 unknown。 */
export function toIpcError(e: unknown): IpcError {
  if (e instanceof IpcError) return e;
  if (e && typeof e === "object" && "code" in e && "message" in e) {
    const body = e as IpcErrorBody;
    return new IpcError(String(body.code), String(body.message));
  }
  return new IpcError("unknown", String(e));
}

export const ipc = {
  getConfig: () => invoke<Config>("get_config"),
  setConfig: (config: Config) => invoke<Config>("set_config", { config }),
  pathsInfo: () => invoke<PathsInfo>("paths_info"),
  /** 读取一次即清空：启动期的问题只提示一次 */
  takeStartupWarning: () => invoke<string | null>("take_startup_warning"),

  authStatus: () => invoke<AccountView | null>("auth_status"),
  /** 玩家名 + 该玩家的启动器独立密码（不是网站主密码，见 ADR-0012） */
  authLogin: (playerName: string, password: string) =>
    invoke<AccountView>("auth_login", { playerName, password }),
  authLogout: () => invoke<void>("auth_logout"),
  authCheck: () => invoke<AccountView | null>("auth_check"),

  /** 皮肤纹理的 data: URL；无皮肤返回 null（前端用默认模型） */
  skinDataUrl: (name: string) => invoke<string | null>("mirror_skin_data_url", { name }),

  hoardSearch: (q: string, offset: number, limit: number) =>
    invoke<SearchResult<HoardProject>>("hoard_search", { q, offset, limit }),
  hoardVersions: (projectId: string) =>
    invoke<HoardVersion[]>("hoard_versions", { projectId }),
};
