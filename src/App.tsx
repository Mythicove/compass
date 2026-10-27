import { createContext, useCallback, useContext, useEffect, useState } from "react";
import { I18nProvider, detectLocale, errorKey, useI18n, type Locale } from "./i18n";
import { ipc, toIpcError, type AccountView, type Config } from "./lib/ipc";
import { HomeView } from "./views/home";
import { InstancesView } from "./views/instances";
import { ModsView } from "./views/mods";
import { SettingsView } from "./views/settings";
import "./App.css";

type View = "home" | "instances" | "mods" | "settings";

const VIEWS: { id: View; labelKey: string }[] = [
  { id: "home", labelKey: "nav.home" },
  { id: "instances", labelKey: "nav.instances" },
  { id: "mods", labelKey: "nav.mods" },
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
        {view === "instances" && <InstancesView />}
        {view === "mods" && <ModsView />}
        {view === "settings" && <SettingsView />}
      </main>
    </div>
  );
}

export default function App() {
  const [config, setConfig] = useState<Config | null>(null);
  const [locale, setLocale] = useState<Locale>(() => detectLocale(""));

  useTheme(config?.theme);

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
