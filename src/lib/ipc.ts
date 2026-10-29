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

/** 版本清单条目（piston-meta）。字段与 Rust 侧 `meta::ManifestEntry` 一一对应，
 *  两边由 tests/shape_contract.rs 比对——改一边不改另一边会在那里失败。 */
export interface ManifestEntry {
  id: string;
  /** Rust 侧字段名是 `kind`，序列化成 `type`（`type` 是 TS 关键字，但作为属性名合法） */
  type: string;
  url: string;
  time: string;
  releaseTime: string;
  sha1?: string;
  complianceLevel: number;
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
  /** null = 原版。装载 loader 时写入，不是从 versionId 反推的（见 Rust 侧 InstanceLoader） */
  loader: InstanceLoader | null;
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
  /** 必需依赖，每项形如 `"<modId> <版本约束>"`（约束为任意版本时只有 modId）。
   *  已排序去重；只展示，不做版本区间求值。 */
  depends: string[];
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

/** Mod loader 类型。Forge 的版本号是 `<游戏版本>-<forge版本>`（如 `1.21.4-54.1.22`），
 *  与 NeoForge 的 `21.4.161` 不同构，UI 直接原样显示。 */
export type LoaderKind = "fabric" | "quilt" | "neoforge" | "forge";

export const LOADER_KINDS: LoaderKind[] = ["fabric", "quilt", "neoforge", "forge"];

/** 专有名词，不翻译（所以放在这里而不是 i18n 表里）。 */
export const LOADER_LABELS: Record<LoaderKind, string> = {
  fabric: "Fabric",
  quilt: "Quilt",
  neoforge: "NeoForge",
  forge: "Forge",
};

/** 一个可用的 loader 版本。`stable` 为 null 表示**来源没说**
 *  （Quilt 的 meta 没有这个字段）——不要把它显示成"开发版"。 */
export interface LoaderVersion {
  version: string;
  stable: boolean | null;
}

/** 实例上记录的 loader。装载时写入，不是从版本 id 反推的。 */
export interface InstanceLoader {
  kind: LoaderKind;
  version: string;
  gameVersion: string;
}

/** 导出结果（对应 Rust 的 `archive::ZipSummary`）。 */
export interface ZipSummary {
  files: number;
  bytes: number;
}

/** 导入前对压缩包的"看一眼"（对应 Rust 的 `instance::ArchiveInfo`）。
 *  这些字段会影响启动行为，UI 必须在导入前原样摊开给用户看。 */
export interface ArchiveInfo {
  name: string;
  versionId: string;
  memoryMb: number | null;
  files: number;
  bytes: number;
  extraJvmArgs: string[];
  extraGameArgs: string[];
  server: ServerJoin | null;
  demo: boolean;
  /** 包里带了别人机器上的 Java 路径；导入时会被清空 */
  hadCustomJava: boolean;
}

/** 装之前对 `.mrpack` 的"看一眼"（对应 Rust 的 `modpack::ModpackInfo`）。
 *  整合包决定我们下载什么、往游戏目录哪里写、装哪个 loader 版本，
 *  所以这些数字与警告必须在落盘之前摊开给用户看。 */
export interface ModpackInfo {
  name: string;
  /** 整合包自己的版本号（不是游戏版本），如 `8.2.0` */
  packVersion: string;
  summary: string | null;
  formatVersion: number;
  /** null = 包里没写、或写成了 `*`，这种包装不了 */
  minecraft: string | null;
  loader: LoaderKind | null;
  loaderVersion: string | null;
  filesTotal: number;
  filesRequired: number;
  /** 作者标为可选，我们仍然装上，但单独报出来 */
  filesOptional: number;
  /** `env.client = unsupported`，客户端安装会跳过 */
  filesSkipped: number;
  bytes: number;
  overrideEntries: number;
  warnings: string[];
}

/** 一次整合包安装的结果（对应 Rust 的 `modpack::ModpackInstall`）。 */
export interface ModpackInstall {
  instance: Instance;
  downloaded: number;
  bytes: number;
  optionalInstalled: number;
  skippedUnsupported: number;
  overrideFiles: number;
  /** 被整合包覆盖掉的已存在文件（相对游戏目录） */
  overwritten: string[];
  failed: string[];
  warnings: string[];
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
  /** 装进实例的内容目录。校验和由 Rust 从 file.hashes 里挑，不在前端挑 */
  hoardInstall: (slug: string, kind: ContentKind, file: HoardFile) =>
    invoke<ContentItem>("hoard_install", { slug, kind, file }),

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
  /** 导出为 zip；dest 来自原生保存对话框 */
  instanceExport: (slug: string, dest: string, includeSaves: boolean) =>
    invoke<ZipSummary>("instance_export", { slug, dest, includeSaves }),
  /** 只读压缩包里的 instance.json，不落盘；UI 据此在导入前让用户确认 */
  instanceInspectArchive: (path: string) =>
    invoke<ArchiveInfo>("instance_inspect_archive", { path }),
  /** slug 由调用方给，不取自压缩包 */
  instanceImport: (path: string, slug: string, name: string) =>
    invoke<Instance>("instance_import", { path, slug, name }),
  /** 只读 `.mrpack` 里的 modrinth.index.json，不落盘；UI 据此在安装前让用户确认 */
  modpackInspect: (path: string) => invoke<ModpackInfo>("modpack_inspect", { path }),
  /** slug 由调用方给，不取自整合包；失败时后端会把实例目录整个删掉 */
  modpackInstall: (path: string, slug: string, name: string) =>
    invoke<ModpackInstall>("modpack_install", { path, slug, name }),
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

  // ---- Mod loader ----
  /** 某个游戏版本可用的 loader 版本（stable 为 null = 来源没说，别当成 dev） */
  loaderVersions: (kind: LoaderKind, gameVersion: string) =>
    invoke<LoaderVersion[]>("loader_versions", { kind, gameVersion }),
  /** 装 loader 并把实例指到新的版本 id；下载进度走 EVENTS.progress */
  loaderInstall: (slug: string, kind: LoaderKind, loaderVersion: string) =>
    invoke<Instance>("loader_install", { slug, kind, loaderVersion }),
  /** 回到原版；原版没装好时会报错而不是把实例指向一个不存在的版本 */
  loaderRemove: (slug: string) => invoke<Instance>("loader_remove", { slug }),

  // ---- 启动 ----
  instanceLaunch: (slug: string) => invoke<LaunchInfo>("instance_launch", { slug }),
  instanceStop: () => invoke<boolean>("instance_stop"),
  gameRunning: () => invoke<string | null>("game_running"),
};
