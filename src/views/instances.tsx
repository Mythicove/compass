import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { errorKey, useI18n } from "../i18n";
import {
  EVENTS,
  ipc,
  toIpcError,
  type FileProgress,
  type GameExit,
  type Instance,
  type JavaRuntime,
  type ManifestEntry,
} from "../lib/ipc";

/** 显示名 → 目录名（必须落在 Rust 侧 validate_slug 的规则内：ASCII 字母数字与 _/-）。 */
export function toSlug(name: string): string {
  const slug = name
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
  return slug || `instance-${Date.now().toString(36)}`;
}

function shortName(url: string): string {
  const last = url.split("/").pop() ?? url;
  return last.length > 46 ? `…${last.slice(-46)}` : last;
}

export function InstancesView() {
  const { t } = useI18n();
  const [instances, setInstances] = useState<Instance[] | null>(null);
  const [installed, setInstalled] = useState<string[]>([]);
  const [versions, setVersions] = useState<ManifestEntry[]>([]);
  const [snapshots, setSnapshots] = useState(false);
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [running, setRunning] = useState<string | null>(null);
  const [java, setJava] = useState<Record<string, JavaRuntime | null>>({});
  // 下载进度：后端按文件推送，这里累计"已完成多少个 / 多少字节"
  const [done, setDone] = useState<{ files: Set<string>; bytes: number; current: string }>({
    files: new Set(),
    bytes: 0,
    current: "",
  });

  // 建实例表单
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [newVersion, setNewVersion] = useState("");

  const reload = useCallback(async () => {
    const [list, ready, game] = await Promise.all([
      ipc.instanceList().catch(() => [] as Instance[]),
      ipc.versionsInstalled().catch(() => [] as string[]),
      ipc.gameRunning().catch(() => null),
    ]);
    setInstances(list);
    setInstalled(ready);
    setRunning(game);
    const probed = await Promise.all(
      list.map(async (i) => [i.slug, await ipc.instanceJava(i.slug).catch(() => null)] as const),
    );
    setJava(Object.fromEntries(probed));
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    ipc
      .versionsList(snapshots)
      .then(setVersions)
      .catch((e) => setError(t(errorKey(toIpcError(e).code))));
  }, [snapshots, t]);

  useEffect(() => {
    const un = [
      listen<FileProgress>(EVENTS.progress, (e) => {
        const p = e.payload;
        setDone((prev) => {
          const finished = p.totalBytes > 0 && p.doneBytes >= p.totalBytes;
          const files = finished && !prev.files.has(p.url) ? new Set(prev.files).add(p.url) : prev.files;
          const bytes =
            finished && !prev.files.has(p.url) ? prev.bytes + p.totalBytes : prev.bytes;
          return { files, bytes, current: shortName(p.url) };
        });
      }),
      listen<GameExit>(EVENTS.gameExit, () => {
        setRunning(null);
        void reload();
      }),
    ];
    return () => {
      void Promise.all(un).then((fs) => fs.forEach((f) => f()));
    };
  }, [reload]);

  const shownVersions = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return (q ? versions.filter((v) => v.id.toLowerCase().includes(q)) : versions).slice(0, 300);
  }, [versions, filter]);

  const act = async (key: string, fn: () => Promise<void>) => {
    if (busy) return;
    setBusy(key);
    setError(null);
    setNotice(null);
    try {
      await fn();
    } catch (e) {
      setError(`${t(errorKey(toIpcError(e).code))} — ${toIpcError(e).message}`);
    } finally {
      setBusy(null);
      setDone({ files: new Set(), bytes: 0, current: "" });
      await reload();
    }
  };

  const create = () =>
    act("create", async () => {
      const slug = toSlug(newName);
      const created = await ipc.instanceCreate(slug, newName.trim() || slug, newVersion);
      setCreating(false);
      setNewName("");
      setNotice(t("instances.created", { name: created.name }));
    });

  const install = (inst: Instance) =>
    act(`${inst.slug}:install`, async () => {
      const r = await ipc.installVersion(inst.versionId);
      setNotice(
        t("instances.installed", {
          version: r.id,
          downloaded: r.download.downloaded,
          skipped: r.download.skipped,
          mb: Math.round(r.download.bytes / 1024 / 1024),
        }),
      );
    });

  const launch = (inst: Instance) =>
    act(`${inst.slug}:launch`, async () => {
      const info = await ipc.instanceLaunch(inst.slug);
      setRunning(info.instance);
      setNotice(t("instances.launched", { java: info.javaVersion }));
    });

  const mb = done.bytes / 1024 / 1024;

  return (
    <div className="stack">
      <section className="card">
        <div className="row between">
          <h2>{t("nav.instances")}</h2>
          <button type="button" onClick={() => setCreating((v) => !v)}>
            {creating ? t("common.cancel") : t("instances.new")}
          </button>
        </div>

        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        {notice && <p className="ok small">{notice}</p>}
        {busy && (
          <p className="muted small" role="status">
            {busy.endsWith(":install")
              ? t("instances.downloading", {
                  files: done.files.size,
                  mb: mb.toFixed(1),
                  current: done.current,
                })
              : t("instances.working")}
          </p>
        )}

        {creating && (
          <div className="stack">
            <label className="field">
              <span>{t("instances.name")}</span>
              <input
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder="1.21.4 Fabric"
              />
            </label>
            <label className="field">
              <span>{t("instances.version")}</span>
              <input
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder={t("instances.filterVersions")}
              />
            </label>
            <select
              size={8}
              value={newVersion}
              onChange={(e) => setNewVersion(e.target.value)}
              aria-label={t("instances.version")}
            >
              {shownVersions.map((v) => (
                <option key={v.id} value={v.id}>
                  {v.id}
                  {v.type === "release" ? "" : ` (${v.type})`}
                </option>
              ))}
            </select>
            <label className="check">
              <input
                type="checkbox"
                checked={snapshots}
                onChange={(e) => setSnapshots(e.target.checked)}
              />
              <span>{t("instances.includeSnapshots")}</span>
            </label>
            <div className="row">
              <button type="button" disabled={busy !== null || !newVersion} onClick={() => void create()}>
                {t("instances.create")}
              </button>
              {newName && <span className="muted small">→ {toSlug(newName)}</span>}
            </div>
          </div>
        )}
      </section>

      {instances && instances.length === 0 && <p className="muted">{t("instances.empty")}</p>}

      {instances?.map((inst) => {
        const ready = installed.includes(inst.versionId);
        const isRunning = running === inst.slug;
        const runtime = java[inst.slug];
        return (
          <section key={inst.slug} className="card">
            <div className="row between">
              <div>
                <strong>{inst.name}</strong>
                <p className="muted small">
                  {inst.versionId} · {t("instances.launches", { n: inst.launches })}
                  {inst.lastLaunchedAt
                    ? ` · ${t("instances.lastPlayed", {
                        time: new Date(inst.lastLaunchedAt * 1000).toLocaleString(),
                      })}`
                    : ""}
                </p>
                <p className="muted small">
                  {runtime
                    ? t("instances.javaFound", { version: runtime.version, path: runtime.path })
                    : t("instances.javaMissing", { version: inst.versionId })}
                </p>
              </div>
              <div className="row">
                {!ready && (
                  <button
                    type="button"
                    disabled={busy !== null}
                    onClick={() => void install(inst)}
                  >
                    {busy === `${inst.slug}:install` ? "…" : t("instances.install")}
                  </button>
                )}
                {isRunning ? (
                  <button type="button" onClick={() => void ipc.instanceStop()}>
                    {t("instances.stop")}
                  </button>
                ) : (
                  <button
                    type="button"
                    disabled={busy !== null || !ready || !runtime}
                    title={ready ? undefined : t("instances.installFirst")}
                    onClick={() => void launch(inst)}
                  >
                    {busy === `${inst.slug}:launch` ? "…" : t("instances.launch")}
                  </button>
                )}
                <button type="button" className="link" disabled={busy !== null} onClick={() => void act(`${inst.slug}:delete`, async () => { await ipc.instanceDelete(inst.slug); })}>
                  {t("instances.delete")}
                </button>
              </div>
            </div>
          </section>
        );
      })}
    </div>
  );
}
