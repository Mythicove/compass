import { mirrorClient, type AuthenticateResponse } from "./api";

const STORAGE_KEY = "compass-auth";

export interface AuthState {
  accessToken: string;
  clientToken: string;
  playerName?: string;
  playerId?: string;
}

export function loadAuth(): AuthState | null {
  const raw = localStorage.getItem(STORAGE_KEY);
  if (!raw) return null;
  try {
    return JSON.parse(raw) as AuthState;
  } catch {
    return null;
  }
}

export function saveAuth(auth: AuthState | null) {
  if (auth) localStorage.setItem(STORAGE_KEY, JSON.stringify(auth));
  else localStorage.removeItem(STORAGE_KEY);
}

function fromResponse(res: AuthenticateResponse): AuthState {
  const profile = res.selectedProfile ?? res.availableProfiles[0];
  return {
    accessToken: res.accessToken,
    clientToken: res.clientToken,
    playerName: profile?.name,
    playerId: profile?.id,
  };
}

// 启动器登录用「玩家名 + 该玩家的独立密码」（ADR-0012）：网站主密码在
// /authserver/authenticate 上无效，站点因此可以关掉账号密码登录而不影响进服。
// 独立密码在网站的账号页（Profile → 启动器登录）里生成，一个玩家可以有多条。
export async function login(playerName: string, password: string): Promise<AuthState> {
  const res = await mirrorClient.authenticate(playerName, password);
  const auth = fromResponse(res);
  saveAuth(auth);
  return auth;
}
