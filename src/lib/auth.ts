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

export async function login(email: string, password: string): Promise<AuthState> {
  const res = await mirrorClient.authenticate(email, password);
  const auth = fromResponse(res);
  saveAuth(auth);
  return auth;
}
