import { createContext, useCallback, useContext, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { I18nProvider, detectLocale, errorKey, useI18n, type Locale } from "./i18n";
import {
  EVENTS,
  ipc,
  toIpcError,
  type AccountView,
  type Config,
  type GameExit,
  type LogLine,
} from "./lib/ipc";
import { HomeView } from "./views/home";
import { InstancesView } from "./views/instances";
import { ContentView } from "./views/content";
import { SettingsView } from "./views/settings";
import { LogsView } from "./views/logs";
import "./App.css";

type View = "home" | "instances" | "content" | "logs" | "settings";

// 游戏日志的模块级环形缓冲：订阅在 App 挂载就开始，早于用户打开日志页——
// 否则启动头几秒的输出（往往正是崩溃原因）会丢。
const LOG_CAP = 2000;
const logState: { lines: LogLine[]; exit: GameExit | null } = { lines: [], exit: null };
const logListeners = new Set<() => void>();

function notifyLogListeners() {
  for (const fn of logListeners) fn();
}

export interface GameLogsSnapshot {
  lines: LogLine[];
  exit: GameExit | null;
}

export function snapshotGameLogs(): GameLogsSnapshot {
  return { lines: logState.lines.slice(), exit: logState.exit };
}

export function clearGameLogs() {
  logState.lines = [];
  logState.exit = null;
  notifyLogListeners();
}

/** LogsView 用它渲染：首帧拿到缓冲快照，之后每次追加/清空都会重新渲染。 */
export function useGameLogs(): GameLogsSnapshot {
  const [snapshot, setSnapshot] = useState(snapshotGameLogs);
  useEffect(
    () => {
      const update = () => setSnapshot(snapshotGameLogs());
      logListeners.add(update);
      update(); // 挂载前的行也要拿到
      return () => {
        logListeners.delete(update);
      };
    },
    [],
  );
  return snapshot;
}

const VIEWS: { id: View; labelKey: string }[] = [
  { id: "home", labelKey: "nav.home" },
  { id: "instances", labelKey: "nav.instances" },
  { id: "content", labelKey: "nav.content" },
  { id: "logs", labelKey: "nav.logs" },
  { id: "settings", labelKey: "nav.settings" },
];

// 设置页读写全局配置；语言与主题变化要立刻反映到整个应用，故提到这一层。
const ConfigContext = createContext<{
  config: Config | null;
  onConfigSaved: (c: Config) => void;
}>({ config: null, onConfigSaved: () => {} });

export function useConfig() {
  return useContext(ConfigContext);
}

/** 把主题落到 <html data-theme>；system 时跟随系统并订阅其变化。 */
function useTheme(theme: Config["theme"] | undefined) {
  useEffect(() => {
    const root = document.documentElement;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const resolved =
        theme === "system" || theme === undefined
          ? media.matches
            ? "dark"
            : "light"
          : theme;
      root.dataset.theme = resolved;
      root.style.colorScheme = resolved;
    };
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);
}

function Shell() {
  const { t } = useI18n();
  const [view, setView] = useState<View>("home");
  // 从实例页点「内容」跳过来时带上是哪个实例；直接点侧栏时为空，由内容页自己选。
  const [contentSlug, setContentSlug] = useState("");
  const [account, setAccount] = useState<AccountView | null>(null);
  const [checking, setChecking] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // 启动时校验一次本地令牌：失效则清掉登录态，网络错误仍算已登录（Rust 侧的语义）
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const current = await ipc.authCheck();
        if (!cancelled) setAccount(current);
      } catch (e) {
        if (!cancelled) setError(t(errorKey(toIpcError(e).code)));
      } finally {
        if (!cancelled) setChecking(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [t]);

  // 切页面时重读账号列表：设置页里可能切了账号、加了离线账号或删了账号，
  // 侧栏与首页要跟着变。用 accountsList 而不是 authCheck——后者会打一次服务端校验，
  // 每次导航都校验既慢又没必要（启动时已经校验过）。依赖里带上 checking：
  // 启动时 authCheck 可能判定令牌失效并删掉账号，等它结束后再读一次才是准的。
  useEffect(() => {
    let cancelled = false;
    void ipc
      .accountsList()
      .then((list) => {
        if (cancelled) return;
        const active = list.accounts.find((a) => a.id === list.activeId);
        setAccount(
          active
            ? {
                playerName: active.playerName,
                playerUuid: active.playerUuid,
                apiUrl: active.apiUrl,
                signedInAt: active.signedInAt,
                kind: active.kind,
              }
            : null,
        );
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [view, checking]);

  const signIn = async (playerName: string, password: string) => {
    setError(null);
    try {
      setAccount(await ipc.authLogin(playerName, password));
    } catch (e) {
      setError(t(errorKey(toIpcError(e).code)));
    }
  };

  const signOut = async () => {
    setError(null);
    try {
      await ipc.authLogout();
      setAccount(null);
    } catch (e) {
      setError(t(errorKey(toIpcError(e).code)));
    }
  };

  return (
    <div className="shell">
      <nav className="sidebar">
        <div className="brand">
          <span className="dot" />
          <span>{t("app.name")}</span>
        </div>
        {VIEWS.map((v) => (
          <button
            key={v.id}
            className={view === v.id ? "nav-item active" : "nav-item"}
            onClick={() => setView(v.id)}
          >
            {t(v.labelKey)}
          </button>
        ))}
        <div className="sidebar-footer">
          {account ? (
            <>
              <span className="who" title={account.playerUuid}>
                {account.playerName}
              </span>
              <button className="link" onClick={() => void signOut()}>
                {t("account.signOut")}
              </button>
            </>
          ) : (
            <span className="muted">{t("account.notSignedIn")}</span>
          )}
        </div>
      </nav>

      <main className="content">
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        {view === "home" && (
          <HomeView account={account} checking={checking} onSignIn={signIn} />
        )}
        {view === "instances" && (
          <InstancesView
            onOpenContent={(slug) => {
              setContentSlug(slug);
              setView("content");
            }}
          />
        )}
        {view === "content" && <ContentView initialSlug={contentSlug} />}
        {view === "logs" && <LogsView />}
        {view === "settings" && <SettingsView />}
      </main>
    </div>
  );
}

export default function App() {
  const [config, setConfig] = useState<Config | null>(null);
  const [locale, setLocale] = useState<Locale>(() => detectLocale(""));

  useTheme(config?.theme);

  // 日志/退出事件在这里订阅（而不是 LogsView）：日志页没打开时输出也进缓冲
  useEffect(() => {
    const un = [
      listen<LogLine>(EVENTS.gameLog, (e) => {
        logState.lines.push(e.payload);
        if (logState.lines.length > LOG_CAP) {
          logState.lines.splice(0, logState.lines.length - LOG_CAP);
        }
        notifyLogListeners();
      }),
      listen<GameExit>(EVENTS.gameExit, (e) => {
        logState.exit = e.payload;
        notifyLogListeners();
      }),
    ];
    return () => {
      void Promise.all(un).then((fs) => fs.forEach((f) => f()));
    };
  }, []);

  useEffect(() => {
    ipc
      .getConfig()
      .then((c) => {
        setConfig(c);
        setLocale(detectLocale(c.language));
      })
      // 配置读不出来也要能进设置页改地址，故不阻塞渲染
      .catch(() => setConfig(null));
  }, []);

  const onConfigSaved = useCallback((c: Config) => {
    setConfig(c);
    setLocale(detectLocale(c.language));
  }, []);

  return (
    <I18nProvider locale={locale}>
      <ConfigContext.Provider value={{ config, onConfigSaved }}>
        <Shell />
      </ConfigContext.Provider>
    </I18nProvider>
  );
}
