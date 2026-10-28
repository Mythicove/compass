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

/** 版本清单条目（piston-meta）。 */
export interface ManifestEntry {
  id: string;
  type: string;
  url: string;
  time: string;
  releaseTime: string;
  sha1?: string;
}

export interface DownloadSummary {
  downloaded: number;
  skipped: number;
  bytes: number;
  failed: string[];
}

export interface InstalledVersion {
  id: string;
  jarVersion: string;
  assetIndexId: string;
  mainClass: string;
  javaMajor: number;
  download: DownloadSummary;
}

export interface Instance {
  slug: string;
  name: string;
  versionId: string;
  createdAt: number;
  lastLaunchedAt: number | null;
  launches: number;
  memoryMb: number | null;
  javaPath: string | null;
  extraJvmArgs: string[];
  extraGameArgs: string[];
  resolution: Resolution | null;
  server: ServerJoin | null;
  demo: boolean;
}

export interface Resolution {
  width: number;
  height: number;
}

export interface ServerJoin {
  host: string;
  port: number | null;
}

/** 设置面板能改的字段。启动计数与最近启动时间由 Rust 侧维护，不在这里回传。 */
export interface InstanceSettings {
  name: string;
  versionId: string;
  memoryMb: number | null;
  javaPath: string | null;
  extraJvmArgs: string[];
  extraGameArgs: string[];
  resolution: Resolution | null;
  server: ServerJoin | null;
  demo: boolean;
}

/** 内容类型，取值就是 Minecraft 的目录名（Rust 侧 `content::Kind`）。 */
export type ContentKind = "mods" | "resourcepacks" | "shaderpacks" | "saves";

export const CONTENT_KINDS: ContentKind[] = [
  "mods",
  "resourcepacks",
  "shaderpacks",
  "saves",
];

/** jar 内解析出的 mod 元数据；解析不出来时为 null，UI 退回显示文件名。 */
export interface ModMeta {
  name: string;
  id: string;
  version: string;
  loaders: string[];
}

export interface ContentItem {
  kind: ContentKind;
  fileName: string;
  displayName: string;
  size: number;
  modifiedAt: number;
  enabled: boolean;
  meta: ModMeta | null;
}

export interface ContentStats {
  kind: ContentKind;
  total: number;
  enabled: number;
  bytes: number;
}

export interface ContentListing {
  kind: ContentKind;
  /** 内容目录绝对路径，交给 opener 插件的 revealItemInDir */
  dir: string;
  items: ContentItem[];
  stats: ContentStats;
}

export interface JavaRuntime {
  path: string;
  version: number;
  is64bit: boolean;
}

export interface LaunchInfo {
  instance: string;
  versionId: string;
  java: string;
  javaVersion: number;
}

/** 事件载荷（Rust 侧经 Tauri event 推送）。 */
export interface FileProgress {
  url: string;
  doneBytes: number;
  totalBytes: number;
}

export interface LogLine {
  stream: "stdout" | "stderr" | string;
  line: string;
}

export interface GameExit {
  instance: string;
  /** null = 用户主动停止（与"游戏崩了"必须能区分） */
  code: number | null;
}

/** 复制实例的进度（字节）。与下载进度分开：那边按文件计数，这边按字节。 */
export interface CopyProgress {
  slug: string;
  copiedBytes: number;
  totalBytes: number;
}

export const EVENTS = {
  progress: "compass://progress",
  gameLog: "compass://game-log",
  gameExit: "compass://game-exit",
  copyProgress: "compass://copy-progress",
} as const;

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

  // ---- 版本与安装 ----
  versionsList: (includeSnapshots: boolean) =>
    invoke<ManifestEntry[]>("versions_list", { includeSnapshots }),
  versionsInstalled: () => invoke<string[]>("versions_installed"),
  /** 装到全局缓存（幂等）；进度经 EVENTS.progress 推送 */
  installVersion: (versionId: string) => invoke<InstalledVersion>("install_version", { versionId }),

  // ---- 实例 ----
  instanceList: () => invoke<Instance[]>("instance_list"),
  instanceCreate: (slug: string, name: string, versionId: string) =>
    invoke<Instance>("instance_create", { slug, name, versionId }),
  instanceDelete: (slug: string) => invoke<void>("instance_delete", { slug }),
  /** 保存实例设置；校验规则在 Rust 侧（内存/分辨率范围、空参数、换行） */
  instanceUpdate: (slug: string, settings: InstanceSettings) =>
    invoke<Instance>("instance_update", { slug, settings }),
  /** 复制实例；进度经 EVENTS.copyProgress 推送 */
  instanceDuplicate: (slug: string, newSlug: string, newName: string) =>
    invoke<Instance>("instance_duplicate", { slug, newSlug, newName }),
  /** 这个实例会用哪个 Java；null = 没找到（此时启动会失败，UI 要提前说清楚） */
  instanceJava: (slug: string) => invoke<JavaRuntime | null>("instance_java", { slug }),

  // ---- 实例内容（mods / 资源包 / 光影 / 存档）----
  contentList: (slug: string, kind: ContentKind) =>
    invoke<ContentListing>("content_list", { slug, kind }),
  /** 只有 mods 支持启停：资源包/光影/存档的启用状态由游戏自己的配置文件决定 */
  contentSetEnabled: (slug: string, fileName: string, enabled: boolean) =>
    invoke<ContentItem>("content_set_enabled", { slug, fileName, enabled }),
  /** sources 是原生文件选择器给的绝对路径 */
  contentImport: (slug: string, kind: ContentKind, sources: string[]) =>
    invoke<string[]>("content_import", { slug, kind, sources }),
  contentRemove: (slug: string, kind: ContentKind, fileName: string) =>
    invoke<void>("content_remove", { slug, kind, fileName }),

  // ---- 启动 ----
  instanceLaunch: (slug: string) => invoke<LaunchInfo>("instance_launch", { slug }),
  instanceStop: () => invoke<boolean>("instance_stop"),
  gameRunning: () => invoke<string | null>("game_running"),
};
