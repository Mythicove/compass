import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { hoardClient, mirrorClient, type HoardProject, type HoardVersion } from "./lib/api";
import { login, loadAuth, saveAuth, type AuthState } from "./lib/auth";
import { SkinPreview } from "./components/skin-preview";
import "./App.css";

const LAUNCHER_KEY = "compass-launcher";
const DEFAULT_LAUNCHER = "prismlauncher";

function App() {
  const [auth, setAuth] = useState<AuthState | null>(null);
  const [playerName, setPlayerName] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [mods, setMods] = useState<HoardProject[]>([]);
  const [versions, setVersions] = useState<Record<string, HoardVersion[]>>({});
  const [launcher, setLauncher] = useState(
    () => localStorage.getItem(LAUNCHER_KEY) ?? DEFAULT_LAUNCHER,
  );
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    setAuth(loadAuth());
    hoardClient
      .search()
      .then((r) => setMods(r.hits))
      .catch(() => {});
  }, []);

  const doLogin = async () => {
    setError(null);
    try {
      setAuth(await login(playerName, password));
    } catch (e) {
      setError(String(e));
    }
  };

  const logout = () => {
    saveAuth(null);
    setAuth(null);
  };

  const saveLauncher = (v: string) => {
    setLauncher(v);
    localStorage.setItem(LAUNCHER_KEY, v);
  };

  const loadVersions = async (m: HoardProject) => {
    if (versions[m.id]) return;
    try {
      const vs = await hoardClient.versions(m.id);
      setVersions((prev) => ({ ...prev, [m.id]: vs }));
    } catch {}
  };

  const downloadAndLaunch = async (m: HoardProject, v: HoardVersion) => {
    setError(null);
    setStatus(null);
    try {
      const url = hoardClient.mrpackUrl(m.id, v.id);
      const dest = `${m.slug}-${v.version_number}.mrpack`;
      await invoke("download_file", { url, dest });
      setStatus(`Downloaded ${dest}. Importing into ${launcher}…`);
      await invoke("launch", { program: launcher, args: ["--import", dest] });
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <main className="app">
      <header className="header">
        <div className="brand">
          <span className="dot" />
          <span>Mythic Compass</span>
        </div>
        <div className="auth">
          {auth ? (
            <>
              <span className="player">{auth.playerName}</span>
              <button onClick={logout}>Sign out</button>
            </>
          ) : (
            <span>Not signed in</span>
          )}
        </div>
      </header>

      <section className="grid">
        <div className="panel">
          <h2>Sign in</h2>
          {!auth ? (
            <>
              <input
                placeholder="Player name"
                value={playerName}
                onChange={(e) => setPlayerName(e.target.value)}
              />
              <input
                type="password"
                placeholder="Launcher password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
              <button onClick={doLogin}>Sign in via Yggdrasil</button>
              <p className="hint">
                Use the player name plus its launcher password — not your site password. Create one
                on the site under Profile → Launcher sign-in.
              </p>
              {error && <p className="error">{error}</p>}
            </>
          ) : (
            <p className="ok">Signed in as {auth.playerName}.</p>
          )}
        </div>

        <div className="panel preview">
          <h2>Skin preview</h2>
          <SkinPreview skinUrl={auth?.playerName ? mirrorClient.skinUrl(auth.playerName) : "/steve.png"} />
        </div>
      </section>

      <section className="panel">
        <h2>Modpacks & mods</h2>
        <div className="mods">
          {mods.map((m) => (
            <div key={m.id} className="mod" onClick={() => loadVersions(m)}>
              <div className="mod-title">
                <span>{m.title}</span>
                <span className="badge">{m.project_type}</span>
              </div>
              <p className="desc">{m.description}</p>
              {(m.loaders ?? []).join(" · ")}
              {versions[m.id] && (
                <div className="versions">
                  {versions[m.id].map((v) => (
                    <button key={v.id} onClick={() => downloadAndLaunch(m, v)}>
                      {v.version_number} (.mrpack)
                    </button>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
      </section>

      <section className="panel">
        <h2>Launch</h2>
        <label>
          Launcher command
          <input value={launcher} onChange={(e) => saveLauncher(e.target.value)} />
        </label>
        <p className="hint">
          Uses an external launcher (Prism Launcher by default) to import the .mrpack and start
          Minecraft. Add your skin server to the launcher's authlib-injector settings to load skins.
        </p>
        {status && <p className="ok">{status}</p>}
        {error && <p className="error">{error}</p>}
      </section>
    </main>
  );
}

export default App;
