// 单 API 域名（皮肤站 + Mod 站，经同一反代）。
const DEFAULT_API_URL = "http://localhost:8080";

export function apiUrl(): string {
  return localStorage.getItem("compass-api-url") ?? DEFAULT_API_URL;
}

async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${apiUrl()}${path}`, {
    ...init,
    headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
  });
  if (!res.ok) throw new Error(`API ${res.status}: ${await res.text()}`);
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export interface Player {
  id: string;
  name: string;
}

export interface AuthenticateResponse {
  accessToken: string;
  clientToken: string;
  availableProfiles: Player[];
  selectedProfile?: Player;
}

export interface HoardProject {
  id: string;
  slug: string;
  title: string;
  description: string;
  project_type: string;
  loaders?: string[];
  game_versions?: string[];
  downloads: number;
  published: string;
}

export interface HoardFile {
  hashes: Record<string, string>;
  url: string;
  filename: string;
  primary: boolean;
  size: number;
}

export interface HoardVersion {
  id: string;
  project_id: string;
  version_number: string;
  version_type: string;
  loaders?: string[];
  game_versions?: string[];
  files: HoardFile[];
}

export interface SearchResult<T> {
  hits: T[];
  offset: number;
  limit: number;
  total_hits: number;
}

export const mirrorClient = {
  // username 是玩家名，password 是该玩家的启动器独立密码（不是网站主密码）
  authenticate: (playerName: string, password: string) =>
    api<AuthenticateResponse>("/authserver/authenticate", {
      method: "POST",
      body: JSON.stringify({ username: playerName, password }),
    }),

  skinUrl: (name: string) => `${apiUrl()}/skin/${name}`,
  capeUrl: (name: string) => `${apiUrl()}/cape/${name}`,
};

export const hoardClient = {
  search: async (q = "", offset = 0, limit = 50) => {
    const res = await fetch(
      `${apiUrl()}/api/mods?q=${encodeURIComponent(q)}&offset=${offset}&limit=${limit}`,
    );
    if (!res.ok) throw new Error(`API ${res.status}`);
    return (await res.json()) as SearchResult<HoardProject>;
  },

  versions: async (projectId: string) => {
    const res = await fetch(`${apiUrl()}/v2/project/${projectId}/version`);
    if (!res.ok) throw new Error(`API ${res.status}`);
    return (await res.json()) as HoardVersion[];
  },

  mrpackUrl: (id: string, vid: string) =>
    `${apiUrl()}/v2/project/${id}/version/${vid}/mrpack`,
};
