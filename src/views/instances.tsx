import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { errorKey, useI18n } from "../i18n";
import {
  EVENTS,
  LOADER_KINDS,
  LOADER_LABELS,
  ipc,
  toIpcError,
  type ArchiveInfo,
  type ModpackInfo,
  type CopyProgress,
  type FileProgress,
  type GameExit,
  type Instance,
  type InstanceSettings,
  type JavaRuntime,
  type LoaderKind,
  type LoaderVersion,
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

/** 实例 → 设置表单的初值。启动计数与最近启动时间不在表单里（Rust 侧维护）。 */
function toSettings(inst: Instance): InstanceSettings {
  return {
    name: inst.name,
    versionId: inst.versionId,
    memoryMb: inst.memoryMb,
    javaPath: inst.javaPath,
    extraJvmArgs: inst.extraJvmArgs ?? [],
    extraGameArgs: inst.extraGameArgs ?? [],
    resolution: inst.resolution ?? null,
    server: inst.server ?? null,
    demo: inst.demo ?? false,
  };
}

function mbOf(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(1);
}

export function InstancesView({ onOpenContent }: { onOpenContent: (slug: string) => void }) {
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

  // 设置面板（一次只开一个：同时编辑两个实例的设置，保存时必然有一个是被旧值覆盖的）
  const [editing, setEditing] = useState<string | null>(null);
  const [form, setForm] = useState<InstanceSettings | null>(null);
  // loader 面板
  const [loaderOf, setLoaderOf] = useState<string | null>(null);
  // 复制进度（几个 GB 的实例要复制一会儿，没有反馈时看起来像卡死）
  const [copying, setCopying] = useState<CopyProgress | null>(null);
  const [dupOf, setDupOf] = useState<string | null>(null);
  const [dupName, setDupName] = useState("");
  // 导出面板（要不要带存档得先问清楚：存档通常是实例里最大的部分，也是本人的进度）
  const [exportOf, setExportOf] = useState<string | null>(null);
  const [exportSaves, setExportSaves] = useState(true);
  // .mrpack 的版本号是包自己的（不是游戏版本），留空则退回实例的版本 id
  const [packVersion, setPackVersion] = useState("");
  // 导入确认：先只读压缩包里的 instance.json（不落盘），把会影响启动的内容摊开给用户看
  const [importInfo, setImportInfo] = useState<{ path: string; info: ArchiveInfo } | null>(null);
  const [importName, setImportName] = useState("");
  // 整合包同理：先只读 modrinth.index.json，把"会下载什么、会覆盖什么、装哪个 loader"
  // 摊开给用户看，确认之后才落盘
  const [packInfo, setPackInfo] = useState<{ path: string; info: ModpackInfo } | null>(null);
  const [packName, setPackName] = useState("");

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
      listen<CopyProgress>(EVENTS.copyProgress, (e) => setCopying(e.payload)),
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

  /** 给子组件用的错误上报（与 act 里那条同一套文案规则，别各写一份）。 */
  const fail = useCallback(
    (e: unknown) => {
      const err = toIpcError(e);
      setError(`${t(errorKey(err.code))} — ${err.message}`);
    },
    [t],
  );

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

  const openSettings = (inst: Instance) => {
    setNotice(null);
    setError(null);
    if (editing === inst.slug) {
      setEditing(null);
      setForm(null);
      return;
    }
    setEditing(inst.slug);
    setForm(toSettings(inst));
  };

  const patch = (p: Partial<InstanceSettings>) => setForm((f) => (f ? { ...f, ...p } : f));

  /** 多行文本框 ↔ 参数数组：一行一个，空行在 Rust 侧被丢掉（这里不报错）。 */
  const patchArgs = (key: "extraJvmArgs" | "extraGameArgs", text: string) =>
    patch({ [key]: text.split("\n") } as Partial<InstanceSettings>);

  const saveSettings = () =>
    act(`${editing}:save`, async () => {
      if (!editing || !form) return;
      const saved = await ipc.instanceUpdate(editing, form);
      setForm(toSettings(saved));
      setNotice(t("instances.saved"));
    });

  const pickJava = () =>
    act("pickJava", async () => {
      const picked = await open({
        multiple: false,
        title: t("instances.javaPath"),
        // 不限制扩展名：三平台的可执行文件名各不相同（java / java.exe / 无扩展名的 shim）
      });
      if (typeof picked !== "string") return;
      patch({ javaPath: picked });
      // 不在这里探测：表单还没保存，探测读的是磁盘上的旧值，报出来的版本会是上一次的
      setNotice(t("instances.javaPicked", { path: picked }));
    });

  // 新目录名要避开已有实例，否则第二次复制/导入必然撞上"already exists"。
  const uniqueSlug = (base: string) => {
    const taken = new Set((instances ?? []).map((i) => i.slug));
    if (!taken.has(base)) return base;
    let candidate = `${base}-2`;
    for (let n = 3; taken.has(candidate); n += 1) candidate = `${base}-${n}`;
    return candidate;
  };

  const duplicate = (inst: Instance) =>
    act(`${inst.slug}:duplicate`, async () => {
      setCopying(null);
      const copy = await ipc.instanceDuplicate(
        inst.slug,
        uniqueSlug(`${inst.slug}-copy`),
        dupName.trim(),
      );
      setDupOf(null);
      setDupName("");
      setCopying(null);
      setNotice(t("instances.duplicated", { name: copy.name }));
    });

  const exportInstance = (inst: Instance) =>
    act(`${inst.slug}:export`, async () => {
      const dest = await save({
        defaultPath: `${inst.slug}.zip`,
        filters: [{ name: "Compass instance (.zip)", extensions: ["zip"] }],
        title: t("instances.export"),
      });
      if (!dest) return;
      const summary = await ipc.instanceExport(inst.slug, dest, exportSaves);
      setExportOf(null);
      setNotice(
        t("instances.exported", {
          files: summary.files,
          mb: mbOf(summary.bytes),
          path: dest,
        }),
      );
    });

  /** 导出成 .mrpack：有来源簿记的文件只写地址，其余的把字节塞进 overrides/。 */
  const exportModpack = (inst: Instance) =>
    act(`${inst.slug}:exportPack`, async () => {
      const dest = await save({
        defaultPath: `${inst.slug}.mrpack`,
        filters: [{ name: "Modrinth modpack (.mrpack)", extensions: ["mrpack"] }],
        title: t("instances.exportModpack"),
      });
      if (!dest) return;
      const out = await ipc.modpackExport(inst.slug, dest, packVersion);
      setExportOf(null);
      const bits = [
        t("instances.exportedModpack", {
          files: out.files,
          overrides: out.overrideFiles,
          mb: mbOf(out.bytes),
          path: dest,
        }),
      ];
      // 簿记过期意味着用户换过文件：那些文件按字节进了包，包会因此变大，
      // 但内容仍然是对的——说清楚比让用户猜"为什么这个包 200MB"好
      if (out.stale.length > 0) {
        bits.push(t("instances.exportStale", { count: out.stale.length }));
      }
      if (out.overrideFiles > 0 && out.files === 0) {
        bits.push(t("instances.exportAllOverrides"));
      }
      setNotice(bits.join(" · "));
    });

  /** 第一步：选包 + 只读地看一眼。看完才谈导入。 */
  const pickImport = () =>
    act("import:pick", async () => {
      const picked = await open({
        multiple: false,
        title: t("instances.import"),
        filters: [{ name: "Compass instance (.zip)", extensions: ["zip"] }],
      });
      if (typeof picked !== "string") return;
      const info = await ipc.instanceInspectArchive(picked);
      setImportInfo({ path: picked, info });
      setImportName(info.name);
    });

  /** 第二步：用户看过内容并确认之后才解压落盘。 */
  const doImport = () =>
    act("import:run", async () => {
      if (!importInfo) return;
      const name = importName.trim();
      const slug = uniqueSlug(toSlug(name || importInfo.info.name));
      const inst = await ipc.instanceImport(importInfo.path, slug, name);
      setImportInfo(null);
      setImportName("");
      setNotice(t("instances.imported", { name: inst.name }));
    });

  /** 第一步：选 .mrpack + 只读地看一眼索引。看完才谈安装。 */
  const pickModpack = () =>
    act("modpack:pick", async () => {
      const picked = await open({
        multiple: false,
        title: t("instances.modpack"),
        filters: [{ name: "Modrinth modpack (.mrpack)", extensions: ["mrpack"] }],
      });
      if (typeof picked !== "string") return;
      const info = await ipc.modpackInspect(picked);
      setPackInfo({ path: picked, info });
      setPackName(info.name);
    });

  /** 第二步：用户看过内容并确认之后才下载落盘。 */
  const doModpackInstall = () =>
    act("modpack:run", async () => {
      if (!packInfo) return;
      const name = packName.trim();
      const slug = uniqueSlug(toSlug(name || packInfo.info.name));
      const res = await ipc.modpackInstall(packInfo.path, slug, name);
      setPackInfo(null);
      setPackName("");
      const bits = [
        t("instances.modpackInstalled", {
          name: res.instance.name,
          downloaded: res.downloaded,
          mb: mbOf(res.bytes),
          overrides: res.overrideFiles,
        }),
      ];
      if (res.optionalInstalled > 0) {
        bits.push(t("instances.modpackOptionalNote", { count: res.optionalInstalled }));
      }
      if (res.overwritten.length > 0) {
        bits.push(
          t("instances.modpackOverwritten", {
            count: res.overwritten.length,
            files: res.overwritten.slice(0, 5).join(", "),
          }),
        );
      }
      if (res.failed.length > 0) {
        bits.push(
          t("instances.modpackFailed", {
            count: res.failed.length,
            files: res.failed.slice(0, 5).join(", "),
          }),
        );
      }
      for (const w of res.warnings) bits.push(w);
      // 体检结果只报数，细节在内容页的「检查依赖与冲突」里：
      // 一行通知里塞几十条 mod id 没人看得完
      if (res.deps.blocked.length + res.deps.missing.length > 0) {
        bits.push(
          t("instances.modpackDeps", {
            blocked: res.deps.blocked.length,
            missing: res.deps.missing.length,
          }),
        );
      }
      setNotice(bits.join(" · "));
    });

  const removeInstance = (inst: Instance) =>
    act(`${inst.slug}:delete`, async () => {
      if (!window.confirm(t("instances.deleteConfirm", { name: inst.name }))) return;
      await ipc.instanceDelete(inst.slug);
      if (editing === inst.slug) {
        setEditing(null);
        setForm(null);
      }
    });

  return (
    <div className="stack">
      <section className="card">
        <div className="row between">
          <h2>{t("nav.instances")}</h2>
          <div className="row">
            <button type="button" className="link" onClick={() => void pickModpack()}>
              {busy === "modpack:pick" ? "…" : t("instances.modpack")}
            </button>
            <button type="button" className="link" onClick={() => void pickImport()}>
              {busy === "import:pick" ? "…" : t("instances.import")}
            </button>
            <button type="button" onClick={() => setCreating((v) => !v)}>
              {creating ? t("common.cancel") : t("instances.new")}
            </button>
          </div>
        </div>

        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        {notice && <p className="ok small">{notice}</p>}
        {busy && (
          <p className="muted small" role="status">
            {busy.endsWith(":install") || busy.endsWith(":loader")
              ? t("instances.downloading", {
                  files: done.files.size,
                  mb: mbOf(done.bytes),
                  current: done.current,
                })
              : copying
                ? t("instances.duplicating", {
                    mb: mbOf(copying.copiedBytes),
                    total: mbOf(copying.totalBytes),
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


      {importInfo && (
        <section className="card warn">
          <h2>{t("instances.importTitle")}</h2>
          <p className="muted small">
            {t("instances.importArchive", {
              name: importInfo.info.name,
              version: importInfo.info.versionId,
              files: importInfo.info.files,
              mb: mbOf(importInfo.info.bytes),
            })}
          </p>
          <p className="small">{t("instances.importWarning")}</p>
          <ul className="small audit">
            {importInfo.info.extraJvmArgs.length > 0 && (
              <li>{t("instances.importJvm", { args: importInfo.info.extraJvmArgs.join(" ") })}</li>
            )}
            {importInfo.info.extraGameArgs.length > 0 && (
              <li>{t("instances.importGame", { args: importInfo.info.extraGameArgs.join(" ") })}</li>
            )}
            {importInfo.info.hadCustomJava && <li>{t("instances.importJava")}</li>}
            {importInfo.info.server && (
              <li>
                {t("instances.importServer", {
                  host: `${importInfo.info.server.host}${
                    importInfo.info.server.port ? `:${importInfo.info.server.port}` : ""
                  }`,
                })}
              </li>
            )}
            {importInfo.info.demo && <li>{t("instances.importDemo")}</li>}
            {importInfo.info.extraJvmArgs.length === 0 &&
              importInfo.info.extraGameArgs.length === 0 &&
              !importInfo.info.hadCustomJava &&
              !importInfo.info.server &&
              !importInfo.info.demo && <li>{t("instances.importClean")}</li>}
          </ul>
          <div className="row sub">
            <label className="field inline grow">
              <span>{t("instances.name")}</span>
              <input value={importName} onChange={(e) => setImportName(e.target.value)} />
            </label>
            <button
              type="button"
              disabled={busy !== null || !importName.trim()}
              onClick={() => void doImport()}
            >
              {busy === "import:run" ? "…" : t("instances.importConfirm")}
            </button>
            <button
              type="button"
              className="link"
              onClick={() => {
                setImportInfo(null);
                setImportName("");
              }}
            >
              {t("common.cancel")}
            </button>
            <span className="muted small">
              → {uniqueSlug(toSlug(importName.trim() || importInfo.info.name))}
            </span>
          </div>
        </section>
      )}

      {packInfo && (
        <section className="card warn">
          <h2>{t("instances.modpackTitle")}</h2>
          <p className="muted small">
            {t("instances.modpackSummary", {
              name: packInfo.info.name,
              version: packInfo.info.packVersion,
              game: packInfo.info.minecraft ?? "—",
            })}
          </p>
          <p className="small">{t("instances.modpackWarning")}</p>
          <ul className="small audit">
            {packInfo.info.minecraft === null ? (
              <li>
                {t("instances.modpackUninstallable", {
                  reason: packInfo.info.warnings[0] ?? "minecraft version missing",
                })}
              </li>
            ) : (
              <>
                <li>
                  {packInfo.info.loader
                    ? t("instances.modpackLoader", {
                        loader: LOADER_LABELS[packInfo.info.loader],
                        version: packInfo.info.loaderVersion ?? "",
                      })
                    : t("instances.modpackNoLoader")}
                </li>
                <li>
                  {t("instances.modpackFiles", {
                    required: packInfo.info.filesRequired,
                    optional: packInfo.info.filesOptional,
                    mb: mbOf(packInfo.info.bytes),
                  })}
                </li>
                {packInfo.info.filesSkipped > 0 && (
                  <li>{t("instances.modpackSkipped", { count: packInfo.info.filesSkipped })}</li>
                )}
                <li>
                  {t("instances.modpackOverrides", { count: packInfo.info.overrideEntries })}
                </li>
                {packInfo.info.warnings.map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </>
            )}
          </ul>
          <div className="row sub">
            <label className="field inline grow">
              <span>{t("instances.name")}</span>
              <input value={packName} onChange={(e) => setPackName(e.target.value)} />
            </label>
            <button
              type="button"
              disabled={busy !== null || !packName.trim() || packInfo.info.minecraft === null}
              onClick={() => void doModpackInstall()}
            >
              {busy === "modpack:run" ? "…" : t("instances.modpackConfirm")}
            </button>
            <button
              type="button"
              className="link"
              onClick={() => {
                setPackInfo(null);
                setPackName("");
              }}
            >
              {t("common.cancel")}
            </button>
            <span className="muted small">
              → {uniqueSlug(toSlug(packName.trim() || packInfo.info.name))}
            </span>
          </div>
        </section>
      )}

      {instances && instances.length === 0 && <p className="muted">{t("instances.empty")}</p>}
      {instances?.map((inst) => {
        const ready = installed.includes(inst.versionId);
        const isRunning = running === inst.slug;
        const runtime = java[inst.slug];
        const isEditing = editing === inst.slug;
        const isDuplicating = dupOf === inst.slug;
        const exporting = exportOf === inst.slug;
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
                <button type="button" className="link" onClick={() => onOpenContent(inst.slug)}>
                  {t("instances.content")}
                </button>
                <button
                  type="button"
                  className="link"
                  disabled={busy !== null}
                  onClick={() => openSettings(inst)}
                >
                  {t("instances.settings")}
                </button>
                <button
                  type="button"
                  className="link"
                  disabled={busy !== null || isRunning}
                  onClick={() => setLoaderOf(loaderOf === inst.slug ? null : inst.slug)}
                >
                  {t("instances.loader")}
                </button>
                <button
                  type="button"
                  className="link"
                  disabled={busy !== null || isRunning}
                  onClick={() => {
                    setDupOf(isDuplicating ? null : inst.slug);
                    setDupName(isDuplicating ? "" : `${inst.name} copy`);
                  }}
                >
                  {t("instances.duplicate")}
                </button>
                <button
                  type="button"
                  className="link"
                  disabled={busy !== null || isRunning}
                  onClick={() => setExportOf(exporting ? null : inst.slug)}
                >
                  {t("instances.export")}
                </button>
                <button
                  type="button"
                  className="link"
                  disabled={busy !== null || isRunning}
                  onClick={() => void removeInstance(inst)}
                >
                  {t("instances.delete")}
                </button>
              </div>
            </div>

            {isDuplicating && (
              <div className="row sub">
                <label className="field inline">
                  <span>{t("instances.duplicateName")}</span>
                  <input
                    value={dupName}
                    onChange={(e) => setDupName(e.target.value)}
                    placeholder={`${inst.name} copy`}
                  />
                </label>
                <button
                  type="button"
                  disabled={busy !== null}
                  onClick={() => void duplicate(inst)}
                >
                  {busy === `${inst.slug}:duplicate` ? "…" : t("instances.duplicate")}
                </button>
                <button type="button" className="link" onClick={() => setDupOf(null)}>
                  {t("common.cancel")}
                </button>
                <span className="muted small">→ {uniqueSlug(`${inst.slug}-copy`)}</span>
              </div>
            )}

            {exporting && (
              <div className="stack sub">
                <label className="check">
                  <input
                    type="checkbox"
                    checked={exportSaves}
                    onChange={(e) => setExportSaves(e.target.checked)}
                  />
                  <span>{t("instances.exportWithSaves")}</span>
                </label>
                <p className="muted small">{t("instances.exportHint")}</p>
                <div className="row">
                  <button
                    type="button"
                    disabled={busy !== null}
                    onClick={() => void exportInstance(inst)}
                  >
                    {busy === `${inst.slug}:export` ? "…" : t("instances.export")}
                  </button>
                  <button
                    type="button"
                    disabled={busy !== null}
                    onClick={() => void exportModpack(inst)}
                  >
                    {busy === `${inst.slug}:exportPack` ? "…" : t("instances.exportModpack")}
                  </button>
                  <button type="button" className="link" onClick={() => setExportOf(null)}>
                    {t("common.cancel")}
                  </button>
                </div>
                <label className="field inline">
                  <span>{t("instances.exportModpackVersion")}</span>
                  <input
                    value={packVersion}
                    placeholder={inst.versionId}
                    onChange={(e) => setPackVersion(e.target.value)}
                  />
                </label>
                <p className="muted small">{t("instances.exportModpackHint")}</p>
              </div>
            )}

            {loaderOf === inst.slug && (
              <LoaderPanel
                inst={inst}
                busy={busy}
                run={act}
                onError={fail}
                onDone={(updated) => {
                  setLoaderOf(null);
                  setNotice(
                    updated.loader
                      ? t("instances.loaderInstalled", {
                          kind: LOADER_LABELS[updated.loader.kind],
                          version: updated.loader.version,
                        })
                      : t("instances.loaderRemoved"),
                  );
                  void reload();
                }}
              />
            )}

            {isEditing && form && (
              <SettingsPanel
                form={form}
                installed={installed}
                versions={versions}
                saving={busy === `${inst.slug}:save`}
                onChange={patch}
                onArgsChange={patchArgs}
                onPickJava={() => void pickJava()}
                onSave={() => void saveSettings()}
                onClose={() => {
                  setEditing(null);
                  setForm(null);
                }}
              />
            )}
          </section>
        );
      })}
    </div>
  );
}

interface LoaderPanelProps {
  inst: Instance;
  busy: string | null;
  /** 父组件的 act：所有会改状态的操作都要走它，否则 busy/进度/禁用态全部失效 */
  run: (key: string, fn: () => Promise<void>) => Promise<void>;
  onError: (e: unknown) => void;
  onDone: (updated: Instance) => void;
}

/**
 * Mod loader 面板。
 *
 * 游戏版本不让用户填：它来自实例（已装过 loader 的用记录的 gameVersion，否则 versionId
 * 本身就是游戏版本）。让用户填的话，给 1.21.4 的实例装上 1.20.1 的 loader 要等到
 * 启动时才炸。
 */
function LoaderPanel({ inst, busy, run, onError, onDone }: LoaderPanelProps) {
  const { t } = useI18n();
  const [kind, setKind] = useState<LoaderKind>(inst.loader?.kind ?? "fabric");
  const [versions, setVersions] = useState<LoaderVersion[] | null>(null);
  const [picked, setPicked] = useState(inst.loader?.version ?? "");
  const gameVersion = inst.loader?.gameVersion ?? inst.versionId;
  const installing = busy === `${inst.slug}:loader`;
  const removing = busy === `${inst.slug}:loaderRemove`;

  useEffect(() => {
    let cancelled = false;
    setVersions(null);
    ipc
      .loaderVersions(kind, gameVersion)
      .then((list) => {
        if (cancelled) return;
        setVersions(list);
        // 默认选中：当前已装的 → 第一个 stable → 第一个
        setPicked((cur) => {
          if (cur && list.some((v) => v.version === cur)) return cur;
          return (
            list.find((v) => v.stable === true)?.version ??
            list.find((v) => v.stable === null)?.version ??
            list[0]?.version ??
            ""
          );
        });
      })
      .catch((e) => {
        if (!cancelled) onError(e);
      });
    return () => {
      cancelled = true;
    };
  }, [kind, gameVersion, onError]);

  const install = () =>
    run(`${inst.slug}:loader`, async () => {
      if (!picked) return;
      onDone(await ipc.loaderInstall(inst.slug, kind, picked));
    });

  const remove = () =>
    run(`${inst.slug}:loaderRemove`, async () => {
      onDone(await ipc.loaderRemove(inst.slug));
    });

  return (
    <div className="stack sub">
      <h3>{t("instances.loaderTitle")}</h3>
      {inst.loader ? (
        <p className="small">
          {t("instances.loaderCurrent", {
            kind: LOADER_LABELS[inst.loader.kind],
            version: inst.loader.version,
            game: inst.loader.gameVersion,
          })}
        </p>
      ) : (
        <p className="muted small">{t("instances.loaderNone")}</p>
      )}
      <div className="row">
        {LOADER_KINDS.map((k) => (
          <button
            key={k}
            type="button"
            className={k === kind ? "tab active" : "tab"}
            onClick={() => setKind(k)}
          >
            {LOADER_LABELS[k]}
          </button>
        ))}
      </div>
      <label className="field">
        <span>{t("instances.loaderVersion")}</span>
        <select
          value={picked}
          disabled={versions === null}
          onChange={(e) => setPicked(e.target.value)}
        >
          {versions === null && <option value="">{t("instances.loaderLoading")}</option>}
          {versions?.length === 0 && <option value="">{t("instances.loaderNoVersions", { game: gameVersion })}</option>}
          {versions?.map((v) => (
            <option key={v.version} value={v.version}>
              {v.version}
              {v.stable === true ? ` · ${t("instances.loaderStable")}` : ""}
              {v.stable === false ? ` · ${t("instances.loaderDev")}` : ""}
            </option>
          ))}
        </select>
      </label>
      <p className="muted small">
        {t("instances.loaderHint", { game: gameVersion, id: inst.versionId })}
      </p>
      <div className="row">
        <button type="button" disabled={busy !== null || !picked} onClick={() => void install()}>
          {installing ? "…" : inst.loader ? t("instances.loaderChange") : t("instances.loaderInstall")}
        </button>
        {inst.loader && (
          <button
            type="button"
            className="link"
            disabled={busy !== null}
            onClick={() => void remove()}
          >
            {removing ? "…" : t("instances.loaderRemove")}
          </button>
        )}
      </div>
    </div>
  );
}

interface SettingsPanelProps {
  form: InstanceSettings;
  installed: string[];
  versions: ManifestEntry[];
  saving: boolean;
  onChange: (p: Partial<InstanceSettings>) => void;
  onArgsChange: (key: "extraJvmArgs" | "extraGameArgs", text: string) => void;
  onPickJava: () => void;
  onSave: () => void;
  onClose: () => void;
}

/** 实例设置。校验规则在 Rust 侧（`instance::sanitize_settings`），这里只做输入。 */
function SettingsPanel({
  form,
  installed,
  versions,
  saving,
  onChange,
  onArgsChange,
  onPickJava,
  onSave,
  onClose,
}: SettingsPanelProps) {
  const { t } = useI18n();
  // 已装的排前面：那才是能直接启动的选项
  const known = useMemo(() => {
    const rest = versions.map((v) => v.id).filter((id) => !installed.includes(id));
    return [...installed, ...rest];
  }, [installed, versions]);

  return (
    <div className="stack sub">
      <h3>{t("instances.settingsTitle")}</h3>
      <div className="grid-2">
        <label className="field">
          <span>{t("instances.name")}</span>
          <input
            value={form.name}
            onChange={(e) => onChange({ name: e.target.value })}
          />
        </label>
        <label className="field">
          <span>{t("instances.version")}</span>
          <input
            value={form.versionId}
            list="compass-known-versions"
            onChange={(e) => onChange({ versionId: e.target.value })}
          />
        </label>
        <label className="field">
          <span>{t("instances.memoryMb")}</span>
          <input
            type="number"
            min={512}
            step={512}
            value={form.memoryMb ?? ""}
            onChange={(e) =>
              onChange({ memoryMb: e.target.value === "" ? null : Number(e.target.value) })
            }
          />
        </label>
        <label className="field">
          <span>{t("instances.javaPath")}</span>
          <span className="row">
            <input
              value={form.javaPath ?? ""}
              placeholder="—"
              onChange={(e) => onChange({ javaPath: e.target.value || null })}
            />
            <button type="button" onClick={onPickJava}>
              {t("instances.pickJava")}
            </button>
          </span>
        </label>
      </div>
      <datalist id="compass-known-versions">
        {known.map((id) => (
          <option key={id} value={id} />
        ))}
      </datalist>

      <div className="grid-2">
        <label className="field">
          <span>{t("instances.extraJvm")}</span>
          <textarea
            rows={3}
            value={form.extraJvmArgs.join("\n")}
            onChange={(e) => onArgsChange("extraJvmArgs", e.target.value)}
          />
        </label>
        <label className="field">
          <span>{t("instances.extraGame")}</span>
          <textarea
            rows={3}
            value={form.extraGameArgs.join("\n")}
            onChange={(e) => onArgsChange("extraGameArgs", e.target.value)}
          />
        </label>
      </div>
      <p className="muted small">{t("instances.argsHint")}</p>

      <label className="check">
        <input
          type="checkbox"
          checked={form.resolution !== null}
          onChange={(e) =>
            onChange({
              resolution: e.target.checked ? { width: 1920, height: 1080 } : null,
            })
          }
        />
        <span>{t("instances.resolution")}</span>
      </label>
      {form.resolution && (
        <div className="row sub">
          <label className="field inline">
            <span>{t("instances.width")}</span>
            <input
              type="number"
              min={160}
              value={form.resolution.width}
              onChange={(e) =>
                onChange({ resolution: { ...form.resolution!, width: Number(e.target.value) } })
              }
            />
          </label>
          <label className="field inline">
            <span>{t("instances.height")}</span>
            <input
              type="number"
              min={160}
              value={form.resolution.height}
              onChange={(e) =>
                onChange({ resolution: { ...form.resolution!, height: Number(e.target.value) } })
              }
            />
          </label>
        </div>
      )}
      {!form.resolution && <p className="muted small">{t("instances.resolutionAuto")}</p>}

      <label className="check">
        <input
          type="checkbox"
          checked={form.server !== null}
          onChange={(e) =>
            onChange({ server: e.target.checked ? { host: "", port: null } : null })
          }
        />
        <span>{t("instances.server")}</span>
      </label>
      {form.server && (
        <div className="row sub">
          <label className="field inline grow">
            <span>{t("instances.serverHost")}</span>
            <input
              value={form.server.host}
              placeholder="play.example.com"
              onChange={(e) => onChange({ server: { ...form.server!, host: e.target.value } })}
            />
          </label>
          <label className="field inline">
            <span>{t("instances.serverPort")}</span>
            <input
              type="number"
              min={1}
              max={65535}
              value={form.server.port ?? ""}
              onChange={(e) =>
                onChange({
                  server: {
                    ...form.server!,
                    port: e.target.value === "" ? null : Number(e.target.value),
                  },
                })
              }
            />
          </label>
        </div>
      )}

      <label className="check">
        <input
          type="checkbox"
          checked={form.demo}
          onChange={(e) => onChange({ demo: e.target.checked })}
        />
        <span>{t("instances.demo")}</span>
      </label>

      <div className="row">
        <button type="button" disabled={saving} onClick={onSave}>
          {saving ? "…" : t("instances.save")}
        </button>
        <button type="button" className="link" onClick={onClose}>
          {t("common.cancel")}
        </button>
      </div>
    </div>
  );
}
