import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useI18n } from "../i18n";
import { EVENTS, type GameExit, type LogLine } from "../lib/ipc";

/** 只留最近这么多行：游戏日志能跑到几万行，全留在内存里会让界面越来越卡。 */
const MAX_LINES = 1500;

export function LogsView() {
  const { t } = useI18n();
  const [lines, setLines] = useState<LogLine[]>([]);
  const [exit, setExit] = useState<GameExit | null>(null);
  const endRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const un = [
      listen<LogLine>(EVENTS.gameLog, (e) => {
        setLines((prev) => {
          const next = prev.length >= MAX_LINES ? prev.slice(prev.length - MAX_LINES + 1) : prev.slice();
          next.push(e.payload);
          return next;
        });
      }),
      listen<GameExit>(EVENTS.gameExit, (e) => setExit(e.payload)),
    ];
    return () => {
      void Promise.all(un).then((fs) => fs.forEach((f) => f()));
    };
  }, []);

  useEffect(() => {
    endRef.current?.scrollIntoView({ block: "end" });
  }, [lines]);

  return (
    <section className="card">
      <div className="row between">
        <h2>{t("logs.title")}</h2>
        <button type="button" className="link" onClick={() => { setLines([]); setExit(null); }}>
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
