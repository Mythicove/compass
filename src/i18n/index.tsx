// 轻量 i18n：桌面端没有 next-intl，也不为一个启动器引入 i18next。
// 键名与 web 的风格保持一致（errors.* 按后端稳定错误码映射），日后要接 Crowdin 也是同一套键。
import { createContext, useCallback, useContext, useEffect, useMemo, type ReactNode } from "react";

export type Locale = "zh-CN" | "en";

type Messages = Record<string, string>;

const zhCN: Messages = {
  "app.name": "Mythic Compass",
  "nav.home": "首页",
  "nav.instances": "实例",
  "nav.mods": "模组",
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
  "home.noInstances": "还没有实例。原生启动将在下一个里程碑（P1）落地：解析版本、下载资源与库、拉起 Minecraft。",

  "mods.searchPlaceholder": "搜索模组…",
  "mods.search": "搜索",
  "mods.empty": "没有匹配的模组",
  "mods.downloads": "{n} 次下载",
  "mods.versions": "版本",
  "mods.installHint": "安装到实例的功能随 P2/P3 落地（当前里程碑只做浏览）。",

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

  "logs.title": "游戏日志",
  "logs.hint": "实时显示游戏进程的 stdout/stderr（只保留最近 1500 行）。崩溃诊断规则库随 P5 落地。",
  "logs.clear": "清空",
  "logs.empty": "还没有日志。启动一个实例后这里会实时显示游戏输出。",
  "logs.exited": "{instance} 已退出，退出码 {code}。",
  "logs.stopped": "{instance} 已被停止。",

  "settings.title": "设置",
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
  "errors.auth": "登录状态已失效，请重新登录",
  "errors.binding_required": "本站要求先绑定第三方账号，请到网站的账号页完成绑定",
  "errors.password_login_disabled": "本站已关闭账号密码登录（启动器不受影响，请用玩家名 + 独立密码）",
  "errors.unknown": "操作失败",
};

const en: Messages = {
  "app.name": "Mythic Compass",
  "nav.home": "Home",
  "nav.instances": "Instances",
  "nav.mods": "Mods",
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
    "No instances yet. Native launch lands in the next milestone (P1): parse versions, download assets and libraries, start Minecraft.",

  "mods.searchPlaceholder": "Search mods…",
  "mods.search": "Search",
  "mods.empty": "No matching mods",
  "mods.downloads": "{n} downloads",
  "mods.versions": "Versions",
  "mods.installHint": "Installing into an instance lands with P2/P3 (this milestone is browse-only).",

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

  "logs.title": "Game log",
  "logs.hint": "Live stdout/stderr from the game process (last 1500 lines kept). Crash-analysis rules land with P5.",
  "logs.clear": "Clear",
  "logs.empty": "No log yet. Launch an instance and its output shows up here live.",
  "logs.exited": "{instance} exited with code {code}.",
  "logs.stopped": "{instance} was stopped.",

  "settings.title": "Settings",
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
