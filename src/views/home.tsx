import { useEffect, useState, type FormEvent } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useI18n } from "../i18n";
import { useConfig } from "../App";
import { ipc, type AccountView } from "../lib/ipc";
import { SkinPreview } from "../components/skin-preview";

interface Props {
  account: AccountView | null;
  checking: boolean;
  onSignIn: (playerName: string, password: string) => Promise<void>;
}

export function HomeView({ account, checking, onSignIn }: Props) {
  const { t } = useI18n();
  const { config } = useConfig();
  const [playerName, setPlayerName] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [skinUrl, setSkinUrl] = useState<string | null>(null);

  // 皮肤纹理由 Rust 取回并转成 data: URL：webview 不直连后端，CSP 的 img-src 也就不用放行外站
  useEffect(() => {
    let cancelled = false;
    if (!account) {
      setSkinUrl(null);
      return;
    }
    void ipc
      .skinDataUrl(account.playerName)
      .then((url) => {
        if (!cancelled) setSkinUrl(url);
      })
      .catch(() => {
        if (!cancelled) setSkinUrl(null);
      });
    return () => {
      cancelled = true;
    };
  }, [account]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy || !playerName.trim() || !password) return;
    setBusy(true);
    try {
      await onSignIn(playerName.trim(), password);
      setPassword("");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="grid-2">
      <section className="card">
        <h2>{t("account.signIn")}</h2>
        {checking ? (
          <p className="muted">{t("account.checking")}</p>
        ) : account ? (
          <div className="stack">
            <p className="ok">{t("account.signedInAs", { player: account.playerName })}</p>
            <p className="muted small">
              {t("account.since", { time: new Date(account.signedInAt * 1000).toLocaleString() })}
            </p>
            <p className="muted small">{account.apiUrl}</p>
          </div>
        ) : (
          <form className="stack" onSubmit={(e) => void submit(e)}>
            <label className="field">
              <span>{t("account.playerName")}</span>
              <input
                value={playerName}
                onChange={(e) => setPlayerName(e.target.value)}
                autoComplete="username"
                placeholder="Steve"
              />
            </label>
            <label className="field">
              <span>{t("account.launcherPassword")}</span>
              <input
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="current-password"
              />
            </label>
            <p className="muted small">{t("account.signInHint")}</p>
            <button type="submit" disabled={busy || !playerName.trim() || !password}>
              {busy ? "…" : t("account.signIn")}
            </button>
            {config && (
              <button
                type="button"
                className="link"
                onClick={() => void openUrl(`${config.apiUrl}/profile`)}
              >
                {t("account.openSite")}
              </button>
            )}
          </form>
        )}
      </section>

      <section className="card center">
        <h2>{t("home.skinPreview")}</h2>
        <SkinPreview skinUrl={skinUrl} />
      </section>

      <section className="card span-2">
        <h2>{t("home.instances")}</h2>
        <p className="muted">{t("home.noInstances")}</p>
      </section>
    </div>
  );
}
