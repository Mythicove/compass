import { useEffect, useState, type FormEvent } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { errorKey, useI18n } from "../i18n";
import { useConfig } from "../App";
import { ipc, toIpcError, type Config, type PathsInfo } from "../lib/ipc";

const PATH_FIELDS: { key: keyof PathsInfo; label: string }[] = [
  { key: "root", label: "root" },
  { key: "configFile", label: "compass.json" },
  { key: "accountsFile", label: "accounts.json" },
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

  useEffect(() => setForm(config), [config]);

  useEffect(() => {
    void ipc.pathsInfo().then(setPaths).catch(() => setPaths(null));
    // 启动期的问题（如损坏的 compass.json 已回落默认值）只在第一次进设置页时提示一次
    void ipc
      .takeStartupWarning()
      .then(setWarning)
      .catch(() => setWarning(null));
  }, []);

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
