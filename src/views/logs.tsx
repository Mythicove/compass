import { useEffect, useRef } from "react";
import { useI18n } from "../i18n";
import { clearGameLogs, useGameLogs } from "../App";

// 日志的订阅与环形缓冲在 App.tsx（应用启动就开始记录，早于本页挂载）；
// 这里只渲染快照，并通过 useGameLogs 保持实时追加。
export function LogsView() {
  const { t } = useI18n();
  const { lines, exit } = useGameLogs();
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    endRef.current?.scrollIntoView({ block: "end" });
  }, [lines]);

  return (
    <section className="card">
      <div className="row between">
        <h2>{t("logs.title")}</h2>
        <button type="button" className="link" onClick={clearGameLogs}>
          {t("logs.clear")}
        </button>
      </div>
      <p className="muted small">{t("logs.hint")}</p>
      {exit && (
        <p className={exit.code === 0 || exit.code === null ? "ok small" : "error small"} role="status">
          {exit.code === null
            ? t("logs.stopped", { instance: exit.instance })
            : t("logs.exited", { instance: exit.instance, code: exit.code })}
        </p>
      )}
      <div className="log">
        {lines.length === 0 ? (
          <p className="muted small">{t("logs.empty")}</p>
        ) : (
          lines.map((l, i) => (
            <div key={i} className={l.stream === "stderr" ? "log-line err" : "log-line"}>
              {l.line}
            </div>
          ))
        )}
        <div ref={endRef} />
      </div>
    </section>
  );
}
