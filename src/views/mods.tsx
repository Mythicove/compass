import { useEffect, useState } from "react";
import { errorKey, useI18n } from "../i18n";
import { ipc, toIpcError, type HoardProject, type HoardVersion } from "../lib/ipc";

export function ModsView() {
  const { t } = useI18n();
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<HoardProject[] | null>(null);
  const [versions, setVersions] = useState<Record<string, HoardVersion[]>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 进页面就拉一次默认列表（与旧版行为一致），之后按搜索词刷新
  useEffect(() => {
    let cancelled = false;
    void ipc
      .hoardSearch("", 0, 50)
      .then((r) => {
        if (!cancelled) setHits(r.hits);
      })
      .catch((e) => {
        if (!cancelled) setError(t(errorKey(toIpcError(e).code)));
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const search = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await ipc.hoardSearch(q.trim(), 0, 50);
      setHits(r.hits);
    } catch (e) {
      setError(t(errorKey(toIpcError(e).code)));
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
      setError(t(errorKey(toIpcError(e).code)));
    }
  };

  return (
    <div className="stack">
      <section className="card">
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
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
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
    </div>
  );
}
