// 轻量 i18n：桌面端没有 next-intl，也不为一个启动器引入 i18next。
// 键名与 web 的风格保持一致（errors.* 按后端稳定错误码映射），日后要接 Crowdin 也是同一套键。
import { createContext, useCallback, useContext, useEffect, useMemo, type ReactNode } from "react";

export type Locale = "zh-CN" | "en";

type Messages = Record<string, string>;

const zhCN: Messages = {
  "app.name": "Mythic Compass",
  "nav.home": "首页",
  "nav.instances": "实例",
  "nav.settings": "设置",

  "account.signIn": "登录",
  "account.signOut": "退出登录",
  "account.playerName": "玩家名",
  "account.launcherPassword": "启动器独立密码",
  "account.signInHint":
    "用玩家名 + 该玩家的独立密码登录，不是网站主密码。独立密码在网站的账号页（Profile → 启动器登录）生成。",
  "account.signedInAs": "已登录：{player}",
  "account.notSignedIn": "未登录",
  "account.since": "登录于 {time}",
  "account.checking": "正在校验令牌…",
  "account.openSite": "在网站上管理独立密码",

  "home.skinPreview": "皮肤预览",
  "home.instances": "我的实例",
  "home.noInstances": "还没有实例。到「实例」页新建一个，装好版本即可启动。",

  "nav.content": "内容",

  "content.title": "实例内容",
  "content.instance": "实例",
  "content.noInstance": "先创建一个实例，再来管理它的模组、资源包与存档。",
  "content.kind.mods": "模组",
  "content.kind.resourcepacks": "资源包",
  "content.kind.shaderpacks": "光影",
  "content.kind.saves": "存档",
  "content.stats": "{total} 项 · 启用 {enabled} · {mb} MB",
  "content.empty": "这个目录还是空的。点「导入」选择文件，或把文件拖进窗口。",
  "content.import": "导入",
  "content.imported": "已导入：{names}",
  "content.openDir": "打开目录",
  "content.enable": "启用",
  "content.disable": "禁用",
  "content.remove": "删除",
  "content.removeConfirm": "删除「{name}」？这个操作无法撤销。",
  "content.removed": "已删除 {name}",
  "content.col.name": "名称",
  "content.col.id": "ID",
  "content.col.version": "版本",
  "content.col.loader": "加载器",
  "content.col.size": "大小",
  "content.col.state": "状态",
  "content.enabled": "已启用",
  "content.disabled": "已禁用",
  "content.enableHint":
    "只有模组能启用/禁用：资源包、光影与存档的开关由游戏自己的配置文件决定，启动器再造一个开关只会显示成已启用而实际没生效。",
  "content.dropHint": "把文件拖到这里即可导入到当前目录。",
  "content.metaUnknown": "读不出元数据，显示文件名",

  "mods.searchPlaceholder": "搜索模组…",
  "mods.search": "搜索",
  "mods.empty": "没有匹配的模组",
  "mods.downloads": "{n} 次下载",
  "mods.online": "从 Hoard 安装",
  "mods.install": "安装",
  "mods.installing": "正在下载：{mb} / {total} MB",
  "mods.installed": "已安装 {name}",
  "mods.installInto": "装到当前实例的「{kind}」目录。",
  "mods.pickInstanceFirst": "先在上面选一个实例，才能把内容装进去。",
  "mods.noSavesInstall": "存档不通过 Hoard 分发；要换世界请用「导入」选一个世界文件夹。",
  "mods.maybeIncompatible": "可能不兼容",
  "mods.incompatible": "这个版本没有标明支持 {game}",
  "mods.incompatibleConfirm":
    "{version} 没有标明支持 {game}，仍要安装吗？版本不对的 mod 通常会让游戏启动即崩。",

  "common.cancel": "取消",
  "nav.logs": "日志",

  "instances.new": "新建实例",
  "instances.name": "名称",
  "instances.version": "游戏版本",
  "instances.filterVersions": "筛选版本…",
  "instances.includeSnapshots": "包含快照版本",
  "instances.create": "创建",
  "instances.empty": "还没有实例。点「新建实例」选一个游戏版本开始。",
  "instances.install": "安装",
  "instances.installFirst": "需要先安装这个版本",
  "instances.launch": "启动",
  "instances.stop": "停止",
  "instances.delete": "删除",
  "instances.launches": "启动过 {n} 次",
  "instances.lastPlayed": "上次 {time}",
  "instances.javaFound": "Java {version}：{path}",
  "instances.javaMissing": "没找到这个版本需要的 Java（装一个 JDK，或在实例里指定路径）",
  "instances.created": "已创建实例「{name}」，接下来点「安装」下载版本文件。",
  "instances.installed": "{version} 安装完成：新下载 {downloaded} 个文件（{mb} MB），跳过已就位 {skipped} 个。",
  "instances.launched": "已启动（Java {java}）。日志见「日志」页。",
  "instances.downloading": "正在下载：{files} 个文件已完成，共 {mb} MB · 当前 {current}",
  "instances.working": "处理中…",
  "instances.content": "内容",
  "instances.settings": "设置",
  "instances.settingsTitle": "实例设置",
  "instances.save": "保存设置",
  "instances.saved": "设置已保存（下次启动生效）",
  "instances.memoryMb": "最大内存（MiB）",
  "instances.javaPath": "Java 可执行文件（留空 = 按版本要求自动探测）",
  "instances.pickJava": "选择…",
  "instances.javaPicked": "已选择：{path}（保存设置后生效）",
  "instances.extraJvm": "附加 JVM 参数（每行一个）",
  "instances.extraGame": "附加游戏参数（每行一个）",
  "instances.argsHint": "参数原样传入：JVM 参数在主类之前，游戏参数排在最后（因此能覆盖版本自带的同名项）。",
  "instances.resolution": "启动分辨率",
  "instances.resolutionAuto": "跟随游戏",
  "instances.width": "宽",
  "instances.height": "高",
  "instances.server": "启动后直连服务器",
  "instances.serverHost": "地址",
  "instances.serverPort": "端口（留空 = 25565）",
  "instances.demo": "演示模式",
  "instances.duplicate": "复制",
  "instances.duplicateName": "副本名称",
  "instances.duplicating": "正在复制：{mb} MB / {total} MB",
  "instances.duplicated": "已复制为「{name}」",
  "instances.deleteConfirm": "删除实例「{name}」？它的世界、模组与设置都会一起消失，且无法撤销。",
  "instances.export": "导出",
  "instances.exportWithSaves": "包含存档",
  "instances.exportHint":
    "不含存档的包适合分享实例（配置 + 模组）；含存档的是完整备份。logs、crash-reports、natives 一律不打包。",
  "instances.exported": "已导出 {files} 个文件（{mb} MB）到 {path}",
  "instances.import": "导入实例",
  "instances.importTitle": "确认导入",
  "instances.importWarning":
    "实例配置会决定启动时执行什么（Java 路径、JVM 与游戏参数）。只在信任来源时继续。",
  "instances.importArchive": "{name} · {version} · {files} 个文件 · {mb} MB",
  "instances.importJvm": "自定义 JVM 参数：{args}",
  "instances.importGame": "自定义游戏参数：{args}",
  "instances.importJava": "指定了 Java 路径（导入后会清空，改为本机自动探测）",
  "instances.importServer": "启动后直连：{host}",
  "instances.importDemo": "演示模式",
  "instances.importClean": "没有自定义启动参数。",
  "instances.importConfirm": "导入",
  "instances.imported": "已导入实例「{name}」",

  "logs.title": "游戏日志",
  "logs.hint": "实时显示游戏进程的 stdout/stderr（只保留最近 2000 行，应用启动即开始记录）。崩溃诊断规则库随 P5 落地。",
  "logs.clear": "清空",
  "logs.empty": "还没有日志。启动一个实例后这里会实时显示游戏输出。",
  "logs.exited": "{instance} 已退出，退出码 {code}。",
  "logs.stopped": "{instance} 已被停止。",

  "settings.servers": "服务器地址",
  "settings.apiUrl": "Mirror（皮肤站 / 账号）",
  "settings.hoardUrl": "Hoard（模组分发）",
  "settings.appearance": "外观",
  "settings.language": "语言",
  "settings.languageAuto": "跟随系统",
  "settings.theme": "主题",
  "settings.themeSystem": "跟随系统",
  "settings.themeDark": "深色",
  "settings.themeLight": "浅色",
  "settings.downloads": "下载",
  "settings.concurrency": "并发数",
  "settings.mirror": "镜像源前缀（留空 = 官方源）",
  "settings.defaults": "默认值",
  "settings.memoryMb": "新实例最大内存（MiB）",
  "settings.save": "保存",
  "settings.saved": "已保存",
  "settings.paths": "数据目录",
  "settings.pathsHint": "资源、库、版本与 JRE 全局共享（多实例不重复下载）；实例各自隔离。",
  "settings.openDataDir": "打开数据目录",
  "settings.startupWarning": "启动提示",

  "errors.network": "网络异常，请检查后端地址与连通性",
  "errors.api": "后端返回错误",
  "errors.invalid": "输入不合法",
  "errors.config": "配置读写失败",
  "errors.io": "本地文件读写失败",
  "errors.not_found": "找不到它（可能已被删除、移动，或这个版本还没安装）",
  "errors.auth": "登录状态已失效，请重新登录",
  "errors.binding_required": "本站要求先绑定第三方账号，请到网站的账号页完成绑定",
  "errors.password_login_disabled": "本站已关闭账号密码登录（启动器不受影响，请用玩家名 + 独立密码）",
  "errors.unknown": "操作失败",
};

const en: Messages = {
  "app.name": "Mythic Compass",
  "nav.home": "Home",
  "nav.instances": "Instances",
  "nav.settings": "Settings",

  "account.signIn": "Sign in",
  "account.signOut": "Sign out",
  "account.playerName": "Player name",
  "account.launcherPassword": "Launcher password",
  "account.signInHint":
    "Sign in with a player name and that player's launcher password — not your site password. Create one on the site under Profile → Launcher sign-in.",
  "account.signedInAs": "Signed in as {player}",
  "account.notSignedIn": "Not signed in",
  "account.since": "Signed in at {time}",
  "account.checking": "Verifying token…",
  "account.openSite": "Manage launcher passwords on the site",

  "home.skinPreview": "Skin preview",
  "home.instances": "My instances",
  "home.noInstances":
    "No instances yet. Create one on the Instances page — install a version and launch it.",

  "nav.content": "Content",

  "content.title": "Instance content",
  "content.instance": "Instance",
  "content.noInstance": "Create an instance first, then manage its mods, packs and worlds.",
  "content.kind.mods": "Mods",
  "content.kind.resourcepacks": "Resource packs",
  "content.kind.shaderpacks": "Shaders",
  "content.kind.saves": "Worlds",
  "content.stats": "{total} items · {enabled} enabled · {mb} MB",
  "content.empty": "Nothing here yet. Click “Import”, or drop files onto the window.",
  "content.import": "Import",
  "content.imported": "Imported: {names}",
  "content.openDir": "Open folder",
  "content.enable": "Enable",
  "content.disable": "Disable",
  "content.remove": "Delete",
  "content.removeConfirm": "Delete “{name}”? This cannot be undone.",
  "content.removed": "Deleted {name}",
  "content.col.name": "Name",
  "content.col.id": "ID",
  "content.col.version": "Version",
  "content.col.loader": "Loader",
  "content.col.size": "Size",
  "content.col.state": "State",
  "content.enabled": "Enabled",
  "content.disabled": "Disabled",
  "content.enableHint":
    "Only mods can be toggled: resource packs, shaders and worlds are switched by the game's own config files, so a launcher-side toggle would read as enabled without taking effect.",
  "content.dropHint": "Drop files here to import them into the current folder.",
  "content.metaUnknown": "metadata unreadable — showing the file name",

  "mods.searchPlaceholder": "Search mods…",
  "mods.search": "Search",
  "mods.empty": "No matching mods",
  "mods.downloads": "{n} downloads",
  "mods.online": "Install from Hoard",
  "mods.install": "Install",
  "mods.installing": "Downloading: {mb} / {total} MB",
  "mods.installed": "Installed {name}",
  "mods.installInto": "Installs into the current instance's “{kind}” folder.",
  "mods.pickInstanceFirst": "Pick an instance above before installing content into it.",
  "mods.noSavesInstall":
    "Worlds aren't distributed through Hoard; use “Import” with a world folder instead.",
  "mods.maybeIncompatible": "maybe incompatible",
  "mods.incompatible": "This version doesn't list {game} as supported",
  "mods.incompatibleConfirm":
    "{version} doesn't list {game} as supported. Install anyway? A mod for the wrong version usually crashes the game at startup.",

  "common.cancel": "Cancel",
  "nav.logs": "Logs",

  "instances.new": "New instance",
  "instances.name": "Name",
  "instances.version": "Game version",
  "instances.filterVersions": "Filter versions…",
  "instances.includeSnapshots": "Include snapshots",
  "instances.create": "Create",
  "instances.empty": "No instances yet. Click “New instance” and pick a game version.",
  "instances.install": "Install",
  "instances.installFirst": "Install this version first",
  "instances.launch": "Launch",
  "instances.stop": "Stop",
  "instances.delete": "Delete",
  "instances.launches": "launched {n}×",
  "instances.lastPlayed": "last played {time}",
  "instances.javaFound": "Java {version}: {path}",
  "instances.javaMissing": "No suitable Java found for this version (install a JDK, or set a path on the instance)",
  "instances.created": "Instance “{name}” created. Click “Install” to download the version files.",
  "instances.installed": "{version} installed: {downloaded} file(s) downloaded ({mb} MB), {skipped} already in place.",
  "instances.launched": "Launched (Java {java}). See the Logs page for output.",
  "instances.downloading": "Downloading: {files} file(s) done, {mb} MB so far · current {current}",
  "instances.working": "Working…",
  "instances.content": "Content",
  "instances.settings": "Settings",
  "instances.settingsTitle": "Instance settings",
  "instances.save": "Save settings",
  "instances.saved": "Settings saved (they apply at the next launch)",
  "instances.memoryMb": "Max memory (MiB)",
  "instances.javaPath": "Java executable (empty = detect from the version's requirement)",
  "instances.pickJava": "Browse…",
  "instances.javaPicked": "Picked: {path} (takes effect once you save)",
  "instances.extraJvm": "Extra JVM arguments (one per line)",
  "instances.extraGame": "Extra game arguments (one per line)",
  "instances.argsHint":
    "Arguments are passed through verbatim: JVM args before the main class, game args last (so they can override the version's own).",
  "instances.resolution": "Window size on launch",
  "instances.resolutionAuto": "Let the game decide",
  "instances.width": "Width",
  "instances.height": "Height",
  "instances.server": "Join a server on launch",
  "instances.serverHost": "Address",
  "instances.serverPort": "Port (empty = 25565)",
  "instances.demo": "Demo mode",
  "instances.duplicate": "Duplicate",
  "instances.duplicateName": "Name for the copy",
  "instances.duplicating": "Copying: {mb} MB of {total} MB",
  "instances.duplicated": "Duplicated as “{name}”",
  "instances.deleteConfirm":
    "Delete instance “{name}”? Its worlds, mods and settings go with it, and this cannot be undone.",
  "instances.export": "Export",
  "instances.exportWithSaves": "Include worlds",
  "instances.exportHint":
    "Without worlds the archive is for sharing an instance (settings + mods); with worlds it's a full backup. logs, crash-reports and natives are never packed.",
  "instances.exported": "Exported {files} file(s) ({mb} MB) to {path}",
  "instances.import": "Import instance",
  "instances.importTitle": "Confirm import",
  "instances.importWarning":
    "An instance's settings decide what runs at launch (Java path, JVM and game arguments). Only continue if you trust where this archive came from.",
  "instances.importArchive": "{name} · {version} · {files} files · {mb} MB",
  "instances.importJvm": "Custom JVM arguments: {args}",
  "instances.importGame": "Custom game arguments: {args}",
  "instances.importJava": "Sets a Java path (cleared on import; this machine detects its own)",
  "instances.importServer": "Joins a server on launch: {host}",
  "instances.importDemo": "Demo mode",
  "instances.importClean": "No custom launch arguments.",
  "instances.importConfirm": "Import",
  "instances.imported": "Imported instance “{name}”",

  "logs.title": "Game log",
  "logs.hint": "Live stdout/stderr from the game process (last 2000 lines kept, recorded from app start). Crash-analysis rules land with P5.",
  "logs.clear": "Clear",
  "logs.empty": "No log yet. Launch an instance and its output shows up here live.",
  "logs.exited": "{instance} exited with code {code}.",
  "logs.stopped": "{instance} was stopped.",

  "settings.servers": "Servers",
  "settings.apiUrl": "Mirror (skin site / accounts)",
  "settings.hoardUrl": "Hoard (mod distribution)",
  "settings.appearance": "Appearance",
  "settings.language": "Language",
  "settings.languageAuto": "System default",
  "settings.theme": "Theme",
  "settings.themeSystem": "System",
  "settings.themeDark": "Dark",
  "settings.themeLight": "Light",
  "settings.downloads": "Downloads",
  "settings.concurrency": "Concurrency",
  "settings.mirror": "Mirror prefix (empty = official sources)",
  "settings.defaults": "Defaults",
  "settings.memoryMb": "Max memory for new instances (MiB)",
  "settings.save": "Save",
  "settings.saved": "Saved",
  "settings.paths": "Data directories",
  "settings.pathsHint":
    "Assets, libraries, versions and JREs are shared globally (instances never re-download them); instances stay isolated.",
  "settings.openDataDir": "Open data directory",
  "settings.startupWarning": "Startup notice",

  "errors.network": "Network error. Check the server address and connectivity",
  "errors.api": "The server returned an error",
  "errors.invalid": "Invalid input",
  "errors.config": "Failed to read or write the configuration",
  "errors.io": "Failed to read or write a local file",
  "errors.not_found": "Not found (it may have been deleted or moved, or that version isn't installed yet)",
  "errors.auth": "Your session expired. Please sign in again",
  "errors.binding_required":
    "This site requires a linked third-party account. Link one on the site's account page",
  "errors.password_login_disabled":
    "Password sign-in is disabled on this site (launchers are unaffected — use a player name and its launcher password)",
  "errors.unknown": "Something went wrong",
};

const tables: Record<Locale, Messages> = { "zh-CN": zhCN, en };

/** 后端错误码 → errors.* 键；未知码落到 errors.api / errors.unknown。 */
export function errorKey(code: string): string {
  const key = `errors.${code}`;
  return key in zhCN ? key : code === "unknown" ? "errors.unknown" : "errors.api";
}

export function detectLocale(preference: string): Locale {
  if (preference === "zh-CN" || preference === "en") return preference;
  const nav = typeof navigator === "undefined" ? "" : navigator.language;
  return nav.toLowerCase().startsWith("zh") ? "zh-CN" : "en";
}

interface I18nValue {
  locale: Locale;
  t: (key: string, vars?: Record<string, string | number>) => string;
}

const I18nContext = createContext<I18nValue | null>(null);

export function I18nProvider({
  locale,
  children,
}: {
  locale: Locale;
  children: ReactNode;
}) {
  // <html lang> 跟随应用语言：屏幕阅读器与浏览器翻译据此判断，静态 index.html 写不死
  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);

  const t = useCallback(
    (key: string, vars?: Record<string, string | number>) => {
      const table = tables[locale];
      let text = table[key] ?? tables.en[key] ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) {
        text = text.split(`{${k}}`).join(String(v));
      }
      return text;
    },
    [locale],
  );
  const value = useMemo(() => ({ locale, t }), [locale, t]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nValue {
  const ctx = useContext(I18nContext);
  if (!ctx) throw new Error("useI18n must be used within I18nProvider");
  return ctx;
}
