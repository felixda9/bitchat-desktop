import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface PublicMessage {
  id: string;
  pubkey: string;
  display: string;
  content: string;
  timestamp: number;
  own: boolean;
  teleported: boolean;
  emote: boolean;
}

export interface Person {
  pubkey: string;
  display: string;
  teleported: boolean;
  me: boolean;
}

export type DmStatus = "sent" | "delivered" | "read" | "failed";

export interface DmMessage {
  id: string;
  from_me: boolean;
  content: string;
  timestamp: number;
  status: DmStatus | null;
}

export interface Conversation {
  peer: string;
  display: string;
  geohash: string;
  unread: number;
  last: string | null;
}

export interface Note {
  id: string;
  display: string;
  content: string;
  timestamp: number;
  own: boolean;
}

export type UiEvent =
  | { type: "identity"; nickname: string; npub: string }
  | { type: "channel"; geohash: string | null; level: string | null; teleported: boolean }
  | { type: "public"; message: PublicMessage }
  | { type: "system"; text: string }
  | { type: "people"; geohash: string; count: number; unknown: boolean; people: Person[] }
  | { type: "conversations"; items: Conversation[] }
  | { type: "dm_opened"; peer: string | null; display: string; messages: DmMessage[] }
  | { type: "dm"; peer: string; message: DmMessage }
  | { type: "dm_status"; peer: string; id: string; status: DmStatus }
  | { type: "clear_timeline" }
  | { type: "clear_dm"; peer: string }
  | { type: "notes"; geohash: string; notes: Note[] }
  | { type: "tor"; state: string; progress: number; summary: string }
  | { type: "relay"; url: string; status: string }
  | {
      type: "settings";
      nickname: string;
      tor_enabled: boolean;
      custom_relays: string[];
      key_in_keychain: boolean;
      has_location: boolean;
    }
  | { type: "wiped" };

export function onEvent(handler: (e: UiEvent) => void): Promise<UnlistenFn> {
  return listen<UiEvent>("bitchat", (e) => handler(e.payload));
}

export const api = {
  join: (geohash: string, manual: boolean) => invoke("join", { geohash, manual }),
  leave: () => invoke("leave"),
  submit: (text: string) => invoke("submit", { text }),
  openDm: (peer: string | null) => invoke("open_dm", { peer }),
  setNickname: (nickname: string) => invoke("set_nickname", { nickname }),
  setTor: (enabled: boolean) => invoke("set_tor", { enabled }),
  addRelay: (url: string) => invoke("add_relay", { url }),
  removeRelay: (url: string) => invoke("remove_relay", { url }),
  setLocation: (lat: number | null, lon: number | null) => invoke("set_location", { lat, lon }),
  wipe: () => invoke("wipe"),
};

const GEOHASH = /^[0-9bcdefghjkmnpqrstuvwxyz]{1,12}$/;

export function cleanGeohash(raw: string): string | null {
  const g = raw.trim().replace(/^#/, "").toLowerCase();
  return GEOHASH.test(g) ? g : null;
}

export function levelForLength(len: number): string {
  if (len <= 2) return "region";
  if (len <= 4) return "province";
  if (len === 5) return "city";
  if (len === 6) return "neighborhood";
  if (len === 7) return "block";
  if (len === 8) return "building";
  return "block";
}

/** Stable per-person colour from the pubkey. */
export function colorFor(pubkey: string): string {
  let h = 0;
  for (let i = 0; i < pubkey.length; i++) h = (h * 31 + pubkey.charCodeAt(i)) >>> 0;
  return `hsl(${h % 360} 70% var(--nick-lightness))`;
}

export function time(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}
