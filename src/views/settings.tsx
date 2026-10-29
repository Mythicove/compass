import { useCallback, useEffect, useState, type FormEvent } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { errorKey, useI18n } from "../i18n";
import { useConfig } from "../App";
import {
  ipc,
  toIpcError,
  type AccountEntry,
  type AccountsView,
  type Config,
  type PathsInfo,
} from "../lib/ipc";

const PATH_FIELDS: { key: keyof PathsInfo; label: string }[] = [
  { key: "root", label: "root" },
  { key: "configFile", label: "compass.json" },
  { key: "accountsFile", label: "accounts.json" },
  { key: "secretsFile", label: "secrets.json" },
  { key: "cache", label: "cache/" },
  { key: "assets", label: "cache/assets/" },
  { key: "libraries", label: "cache/libraries/" },
  { key: "versions", label: "cache/versions/" },
  { key: "java", label: "cache/java/" },
  { key: "tools", label: "cache/tools/" },
  { key: "instances", label: "instances/" },
];

export function SettingsView() {
  const { t } = useI18n();
  const { config, onConfigSaved } = useConfig();
  const [form, setForm] = useState<Config | null>(config);
  const [paths, setPaths] = useState<PathsInfo | null>(null);
  const [warning, setWarning] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [accounts, setAccounts] = useState<AccountsView | null>(null);
  const [offlineName, setOfflineName] = useState("");
  const [accountBusy, setAccountBusy] = useState<string | null>(null);
  const [accountError, setAccountError] = useState<string | null>(null);

  const reloadAccounts = useCallback(async () => {
    try {
      setAccounts(await ipc.accountsList());
    } catch {
      setAccounts(null);
    }
  }, []);

  /** 账号操作的统一包装：一次只允许一个在跑，失败要显示出来说而不是静默。 */
  const accountAct = async (key: string, fn: () => Promise<void>) => {
    if (accountBusy) return;
    setAccountBusy(key);
    setAccountError(null);
    try {
      await fn();
      await reloadAccounts();
    } catch (e) {
      setAccountError(t(errorKey(toIpcError(e).code)));
    } finally {
      setAccountBusy(null);
    }
  };

  const switchAccount = (id: string) =>
    accountAct(`switch:${id}`, async () => {
      await ipc.accountSetActive(id);
    });

  const addOffline = () =>
    accountAct("add-offline", async () => {
      await ipc.accountAddOffline(offlineName.trim());
      setOfflineName("");
    });

  const removeAccount = (a: AccountEntry) =>
    accountAct(`remove:${a.id}`, async () => {
      if (!window.confirm(t("settings.accountRemoveConfirm", { name: a.playerName }))) return;
      await ipc.accountRemove(a.id);
    });

  useEffect(() => setForm(config), [config]);

  useEffect(() => {
    void ipc.pathsInfo().then(setPaths).catch(() => setPaths(null));
    // 启动期的问题（如损坏的 compass.json 已回落默认值）只在第一次进设置页时提示一次
    void ipc
      .takeStartupWarning()
      .then(setWarning)
      .catch(() => setWarning(null));
    void reloadAccounts();
  }, [reloadAccounts]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!form) return;
    setError(null);
    setSaved(false);
    try {
      onConfigSaved(await ipc.setConfig(form));
      setSaved(true);
    } catch (err) {
      setError(t(errorKey(toIpcError(err).code)));
    }
  };

  if (!form) return <p className="muted">{t("errors.config")}</p>;

  const patch = (p: Partial<Config>) => setForm({ ...form, ...p });

  return (
    <form className="stack" onSubmit={(e) => void submit(e)}>
      <section className="card">
        <h2>{t("settings.servers")}</h2>
        <label className="field">
          <span>{t("settings.apiUrl")}</span>
          <input
            value={form.apiUrl}
            onChange={(e) => patch({ apiUrl: e.target.value })}
            spellCheck={false}
          />
        </label>
        <label className="field">
          <span>{t("settings.hoardUrl")}</span>
          <input
            value={form.hoardUrl}
            onChange={(e) => patch({ hoardUrl: e.target.value })}
            spellCheck={false}
          />
        </label>
      </section>

      <section className="card">
        <h2>{t("settings.appearance")}</h2>
        <label className="field">
          <span>{t("settings.language")}</span>
          <select
            value={form.language}
            onChange={(e) => patch({ language: e.target.value })}
          >
            <option value="">{t("settings.languageAuto")}</option>
            <option value="zh-CN">中文（简体）</option>
            <option value="en">English</option>
          </select>
        </label>
        <label className="field">
          <span>{t("settings.theme")}</span>
          <select
            value={form.theme}
            onChange={(e) => patch({ theme: e.target.value as Config["theme"] })}
          >
            <option value="system">{t("settings.themeSystem")}</option>
            <option value="dark">{t("settings.themeDark")}</option>
            <option value="light">{t("settings.themeLight")}</option>
          </select>
        </label>
      </section>

      <section className="card">
        <h2>{t("settings.downloads")}</h2>
        <label className="field">
          <span>{t("settings.concurrency")}</span>
          <input
            type="number"
            min={1}
            max={64}
            value={form.download.concurrency}
            onChange={(e) =>
              patch({
                download: { ...form.download, concurrency: Number(e.target.value) },
              })
            }
          />
        </label>
        <label className="field">
          <span>{t("settings.mirror")}</span>
          <input
            value={form.download.mirror}
            onChange={(e) => patch({ download: { ...form.download, mirror: e.target.value } })}
            spellCheck={false}
            placeholder="https://bmclapi2.bangbang93.com"
          />
        </label>
      </section>

      <section className="card">
        <h2>{t("settings.defaults")}</h2>
        <label className="field">
          <span>{t("settings.memoryMb")}</span>
          <input
            type="number"
            min={512}
            max={65536}
            step={512}
            value={form.defaults.memoryMb}
            onChange={(e) =>
              patch({ defaults: { ...form.defaults, memoryMb: Number(e.target.value) } })
            }
          />
        </label>
      </section>

      <div className="row">
        <button type="submit">{t("settings.save")}</button>
        {saved && <span className="ok">{t("settings.saved")}</span>}
        {error && (
          <span className="error" role="alert">
            {error}
          </span>
        )}
      </div>

      {warning && (
        <section className="card warn">
          <h2>{t("settings.startupWarning")}</h2>
          <p className="small">{warning}</p>
        </section>
      )}

      {accounts && (
        <section className="card">
          <h2>{t("settings.accounts")}</h2>
          <p className="muted small">
            {accounts.store.encrypted
              ? t("settings.accountsStoreKeyring")
              : t("settings.accountsStorePlain", { path: accounts.store.path })}
          </p>
          {accountError && (
            <p className="error small" role="alert">
              {accountError}
            </p>
          )}
          {accounts.accounts.length === 0 ? (
            <p className="muted small">{t("settings.accountsEmpty")}</p>
          ) : (
            <table className="items">
              <thead>
                <tr>
                  <th>{t("settings.accountColName")}</th>
                  <th>{t("settings.accountColKind")}</th>
                  <th>{t("settings.accountColSite")}</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {accounts.accounts.map((a) => (
                  <tr key={a.id} className={a.id === accounts.activeId ? undefined : "off"}>
                    <td>
                      <span title={a.playerUuid}>{a.playerName}</span>
                      {a.id === accounts.activeId && (
                        <em className="muted small"> · {t("settings.accountActive")}</em>
                      )}
                    </td>
                    <td className="muted small">{t(`settings.accountKind.${a.kind}`)}</td>
                    <td className="muted small">{a.apiUrl || "—"}</td>
                    <td>
                      <div className="row">
                        {a.id !== accounts.activeId && (
                          <button
                            type="button"
                            className="link"
                            disabled={accountBusy !== null}
                            onClick={() => void switchAccount(a.id)}
                          >
                            {accountBusy === `switch:${a.id}` ? "…" : t("settings.accountUse")}
                          </button>
                        )}
                        <button
                          type="button"
                          className="link"
                          disabled={accountBusy !== null}
                          onClick={() => void removeAccount(a)}
                        >
                          {accountBusy === `remove:${a.id}` ? "…" : t("settings.accountRemove")}
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          <div className="row sub">
            <label className="field inline grow">
              <span>{t("settings.accountOfflineName")}</span>
              <input
                value={offlineName}
                maxLength={16}
                placeholder="Steve"
                onChange={(e) => setOfflineName(e.target.value)}
              />
            </label>
            <button
              type="button"
              disabled={accountBusy !== null || !offlineName.trim()}
              onClick={() => void addOffline()}
            >
              {accountBusy === "add-offline" ? "…" : t("settings.accountAddOffline")}
            </button>
          </div>
          <p className="muted small">{t("settings.accountOfflineHint")}</p>
        </section>
      )}

      {paths && (
        <section className="card">
          <div className="row between">
            <h2>{t("settings.paths")}</h2>
            <button
              type="button"
              className="link"
              onClick={() => void revealItemInDir(paths.configFile)}
            >
              {t("settings.openDataDir")}
            </button>
          </div>
          <p className="muted small">{t("settings.pathsHint")}</p>
          <dl className="paths">
            {PATH_FIELDS.map((f) => (
              <div key={f.key}>
                <dt>{f.label}</dt>
                <dd>{paths[f.key]}</dd>
              </div>
            ))}
          </dl>
        </section>
      )}
    </form>
  );
}
