import { useCallback, useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { errorKey, useI18n } from "../i18n";
import {
  CONTENT_KINDS,
  ipc,
  toIpcError,
  type ContentKind,
  type ContentListing,
  type HoardProject,
  type HoardVersion,
  type Instance,
} from "../lib/ipc";

function mb(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(1);
}

/** 文件选择器的过滤条件。存档是文件夹（世界就是一个目录），不走扩展名过滤。 */
const FILTERS: Record<ContentKind, { name: string; extensions: string[] } | null> = {
  mods: { name: "Mods", extensions: ["jar", "zip", "litemod", "disabled"] },
  resourcepacks: { name: "Resource packs", extensions: ["zip"] },
  shaderpacks: { name: "Shader packs", extensions: ["zip"] },
  saves: null,
};

export function ContentView({ initialSlug }: { initialSlug?: string }) {
  const { t } = useI18n();
  const [instances, setInstances] = useState<Instance[]>([]);
  const [slug, setSlug] = useState(initialSlug ?? "");
  const [kind, setKind] = useState<ContentKind>("mods");
  const [listing, setListing] = useState<ContentListing | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [dropping, setDropping] = useState(false);

  const fail = useCallback(
    (e: unknown) => setError(`${t(errorKey(toIpcError(e).code))} — ${toIpcError(e).message}`),
    [t],
  );

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const list = await ipc.instanceList().catch(() => [] as Instance[]);
      if (cancelled) return;
      setInstances(list);
      // 从实例页跳过来时已经指定了实例；否则选最近玩过的那个（列表就是这个顺序）
      setSlug((cur) => cur || initialSlug || list[0]?.slug || "");
    })();
    return () => {
      cancelled = true;
    };
  }, [initialSlug]);

  const reload = useCallback(async () => {
    if (!slug) {
      setListing(null);
      return;
    }
    try {
      setListing(await ipc.contentList(slug, kind));
    } catch (e) {
      fail(e);
    }
  }, [slug, kind, fail]);

  useEffect(() => {
    setNotice(null);
    void reload();
  }, [reload]);

  // 拖放导入。Tauri 的 dragDropEnabled（默认开）会接管 HTML5 的 drop 事件，
  // 所以这里用 webview 的原生事件；拿不到路径时静默跳过（文件选择器始终是主路径）。
  useEffect(() => {
    if (!slug) return;
    let un: (() => void) | null = null;
    let cancelled = false;
    void (async () => {
      try {
        const off = await getCurrentWebview().onDragDropEvent((e) => {
          if (e.payload.type === "enter") setDropping(true);
          if (e.payload.type === "leave") setDropping(false);
          if (e.payload.type !== "drop") return;
          setDropping(false);
          const paths = e.payload.paths;
          void act("import", async () => {
            const done = await ipc.contentImport(slug, kind, paths);
            setNotice(t("content.imported", { names: done.join(", ") }));
          });
        });
        if (cancelled) off();
        else un = off;
      } catch {
        // 拖放不可用不影响其它功能
      }
    })();
    return () => {
      cancelled = true;
      un?.();
    };
    // act 每次渲染都是新的，但拖放只关心"当前 slug/kind 下导入"，
    // 把它放进依赖会让监听器每次渲染都重挂（丢事件）。
  }, [slug, kind]);

  const act = async (key: string, fn: () => Promise<void>) => {
    if (busy) return;
    setBusy(key);
    setError(null);
    try {
      await fn();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(null);
      await reload();
    }
  };

  const pickAndImport = () =>
    act("import", async () => {
      const filter = FILTERS[kind];
      const picked = await open(
        kind === "saves"
          ? { directory: true, multiple: false, title: t("content.import") }
          : {
              multiple: true,
              title: t("content.import"),
              ...(filter ? { filters: [filter] } : {}),
            },
      );
      if (!picked) return;
      const sources = Array.isArray(picked) ? picked : [picked];
      if (sources.length === 0) return;
      const done = await ipc.contentImport(slug, kind, sources);
      setNotice(t("content.imported", { names: done.join(", ") }));
    });

  const toggle = (item: { fileName: string; enabled: boolean }) =>
    act(`toggle:${item.fileName}`, async () => {
      await ipc.contentSetEnabled(slug, item.fileName, !item.enabled);
    });

  const remove = (name: string) =>
    act(`remove:${name}`, async () => {
      if (!window.confirm(t("content.removeConfirm", { name }))) return;
      await ipc.contentRemove(slug, kind, name);
      setNotice(t("content.removed", { name }));
    });

  const stats = listing?.stats;

  return (
    <div className="stack">
      <section className="card">
        <div className="row between">
          <h2>{t("content.title")}</h2>
          <div className="row">
            <label className="field inline">
              <span>{t("content.instance")}</span>
              <select
                value={slug}
                onChange={(e) => setSlug(e.target.value)}
                aria-label={t("content.instance")}
              >
                <option value="" />
                {instances.map((i) => (
                  <option key={i.slug} value={i.slug}>
                    {i.name}
                  </option>
                ))}
              </select>
            </label>
          </div>
        </div>

        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        {notice && <p className="ok small">{notice}</p>}

        {!slug ? (
          <p className="muted">{t("content.noInstance")}</p>
        ) : (
          <>
            <div className="row between tabs">
              <div className="row">
                {CONTENT_KINDS.map((k) => (
                  <button
                    key={k}
                    type="button"
                    className={k === kind ? "tab active" : "tab"}
                    onClick={() => setKind(k)}
                  >
                    {t(`content.kind.${k}`)}
                  </button>
                ))}
              </div>
              <div className="row">
                <button type="button" disabled={busy !== null} onClick={() => void pickAndImport()}>
                  {busy === "import" ? "…" : t("content.import")}
                </button>
                <button
                  type="button"
                  className="link"
                  disabled={!listing}
                  onClick={() => listing && void revealItemInDir(listing.dir)}
                >
                  {t("content.openDir")}
                </button>
              </div>
            </div>

            {stats && (
              <p className="muted small">
                {t("content.stats", {
                  total: stats.total,
                  enabled: stats.enabled,
                  mb: mb(stats.bytes),
                })}
              </p>
            )}
            {kind !== "mods" && <p className="muted small">{t("content.enableHint")}</p>}
            <p className="muted small">{t("content.dropHint")}</p>

            {listing && listing.items.length === 0 && <p className="muted">{t("content.empty")}</p>}

            {listing && listing.items.length > 0 && (
              <table className={dropping ? "items dropping" : "items"}>
                <thead>
                  <tr>
                    <th>{t("content.col.name")}</th>
                    <th>{t("content.col.id")}</th>
                    <th>{t("content.col.version")}</th>
                    <th>{t("content.col.loader")}</th>
                    <th className="num">{t("content.col.size")}</th>
                    <th>{t("content.col.state")}</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {listing.items.map((item) => (
                    <tr key={item.fileName} className={item.enabled ? undefined : "off"}>
                      <td>
                        <span title={item.fileName}>{item.displayName}</span>
                        {!item.meta && kind === "mods" && item.fileName.endsWith(".jar") && (
                          <em className="muted small"> · {t("content.metaUnknown")}</em>
                        )}
                      </td>
                      <td className="muted small">{item.meta?.id ?? "—"}</td>
                      <td className="muted small">{item.meta?.version ?? "—"}</td>
                      <td className="muted small">{item.meta?.loaders.join(" + ") || "—"}</td>
                      <td className="num muted small">{mb(item.size)}</td>
                      <td>
                        {kind === "mods" ? (
                          <span className={item.enabled ? "badge" : "badge off"}>
                            {item.enabled ? t("content.enabled") : t("content.disabled")}
                          </span>
                        ) : (
                          <span className="muted small">—</span>
                        )}
                      </td>
                      <td>
                        <div className="row">
                          {kind === "mods" && (
                            <button
                              type="button"
                              className="link"
                              disabled={busy !== null}
                              onClick={() => void toggle(item)}
                            >
                              {busy === `toggle:${item.fileName}`
                                ? "…"
                                : item.enabled
                                  ? t("content.disable")
                                  : t("content.enable")}
                            </button>
                          )}
                          <button
                            type="button"
                            className="link"
                            disabled={busy !== null}
                            onClick={() => void remove(item.fileName)}
                          >
                            {busy === `remove:${item.fileName}` ? "…" : t("content.remove")}
                          </button>
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </>
        )}
      </section>

      <HoardBrowser onError={fail} />
    </div>
  );
}

/** Hoard 浏览（P0 的能力保留在这里）。直接安装到实例是 P2 的最后一步。 */
function HoardBrowser({ onError }: { onError: (e: unknown) => void }) {
  const { t } = useI18n();
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<HoardProject[] | null>(null);
  const [versions, setVersions] = useState<Record<string, HoardVersion[]>>({});
  const [busy, setBusy] = useState(false);

  // 进页面就拉一次默认列表，之后按搜索词刷新
  useEffect(() => {
    let cancelled = false;
    void ipc
      .hoardSearch("", 0, 50)
      .then((r) => {
        if (!cancelled) setHits(r.hits);
      })
      .catch((e) => {
        if (!cancelled) onError(e);
      });
    return () => {
      cancelled = true;
    };
  }, [onError]);

  const search = async () => {
    setBusy(true);
    try {
      const r = await ipc.hoardSearch(q.trim(), 0, 50);
      setHits(r.hits);
    } catch (e) {
      onError(e);
    } finally {
      setBusy(false);
    }
  };

  const loadVersions = async (p: HoardProject) => {
    if (versions[p.id]) return;
    try {
      const list = await ipc.hoardVersions(p.id);
      setVersions((prev) => ({ ...prev, [p.id]: list }));
    } catch (e) {
      onError(e);
    }
  };

  return (
    <>
      <section className="card">
        <h2>{t("mods.online")}</h2>
        <div className="row">
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void search();
            }}
            placeholder={t("mods.searchPlaceholder")}
            aria-label={t("mods.searchPlaceholder")}
          />
          <button onClick={() => void search()} disabled={busy}>
            {busy ? "…" : t("mods.search")}
          </button>
        </div>
        <p className="muted small">{t("mods.installHint")}</p>
      </section>

      {hits && hits.length === 0 && <p className="muted">{t("mods.empty")}</p>}

      {hits?.map((p) => (
        <section
          key={p.id}
          className="card clickable"
          onClick={() => void loadVersions(p)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") void loadVersions(p);
          }}
          role="button"
          tabIndex={0}
        >
          <div className="row between">
            <strong>{p.title}</strong>
            <span className="badge">{p.project_type}</span>
          </div>
          <p className="muted small">{p.description}</p>
          <p className="muted small">
            {(p.loaders ?? []).join(" · ")} — {t("mods.downloads", { n: p.downloads })}
          </p>
          {versions[p.id] && (
            <div className="chips">
              {versions[p.id].map((v) => (
                <span key={v.id} className="chip" title={v.version_type}>
                  {v.version_number}
                </span>
              ))}
            </div>
          )}
        </section>
      ))}
    </>
  );
}
