import type { Conversation, DmMessage, Note, Person, PublicMessage, RegionalChannel, UiEvent } from "./api";

export type Line = { kind: "public"; message: PublicMessage } | { kind: "system"; text: string; timestamp: number };
export type DmLine = { kind: "dm"; message: DmMessage } | { kind: "system"; text: string; timestamp: number };

const TIMELINE_CAP = 1337;

export const app = $state({
  nickname: "",
  npub: "",
  channel: null as null | { geohash: string; level: string; teleported: boolean },
  timeline: [] as Line[],
  people: { count: 0, unknown: false, list: [] as Person[] },
  conversations: [] as Conversation[],
  dm: null as null | { peer: string; display: string; lines: DmLine[] },
  tor: { state: "starting", progress: 0, summary: "" },
  relays: {} as Record<string, string>,
  settings: { tor_enabled: true, custom_relays: [] as string[], key_in_keychain: true, has_location: false },
  notes: [] as Note[],
  regional: [] as RegionalChannel[],
  /** Bluetooth: starting | on | off | unavailable. */
  mesh: { state: "starting", links: 0 },
  wipedAt: 0,
});

function pushTimeline(line: Line) {
  app.timeline.push(line);
  if (app.timeline.length > TIMELINE_CAP) app.timeline.splice(0, app.timeline.length - TIMELINE_CAP);
}

export function handle(e: UiEvent) {
  switch (e.type) {
    case "location":
      app.regional = e.channels;
      break;
    case "identity":
      app.nickname = e.nickname;
      app.npub = e.npub;
      break;
    case "channel":
      app.channel = e.geohash ? { geohash: e.geohash, level: e.level ?? "", teleported: e.teleported } : null;
      break;
    case "public":
      pushTimeline({ kind: "public", message: e.message });
      break;
    case "system": {
      const line = { kind: "system" as const, text: e.text, timestamp: Date.now() };
      if (app.dm) app.dm.lines.push(line);
      else pushTimeline(line);
      break;
    }
    case "people":
      app.people = { count: e.count, unknown: e.unknown, list: e.people };
      break;
    case "conversations":
      app.conversations = e.items;
      break;
    case "dm_opened":
      app.dm = e.peer
        ? { peer: e.peer, display: e.display, lines: e.messages.map((m) => ({ kind: "dm" as const, message: m })) }
        : null;
      break;
    case "dm":
      if (app.dm && app.dm.peer === e.peer) app.dm.lines.push({ kind: "dm", message: e.message });
      break;
    case "dm_status":
      if (app.dm && app.dm.peer === e.peer) {
        for (const l of app.dm.lines) if (l.kind === "dm" && l.message.id === e.id) l.message.status = e.status;
      }
      break;
    case "clear_timeline":
      app.timeline = [];
      break;
    case "clear_dm":
      if (app.dm && app.dm.peer === e.peer) app.dm.lines = [];
      break;
    case "notes":
      app.notes = e.notes;
      break;
    case "tor":
      app.tor = { state: e.state, progress: e.progress, summary: e.summary };
      break;
    case "relay":
      app.relays[e.url] = e.status;
      break;
    case "settings":
      app.settings = {
        tor_enabled: e.tor_enabled,
        custom_relays: e.custom_relays,
        key_in_keychain: e.key_in_keychain,
        has_location: e.has_location,
      };
      break;
    case "mesh":
      app.mesh = { state: e.state, links: e.links };
      break;
    case "wiped":
      app.timeline = [];
      app.dm = null;
      app.conversations = [];
      app.notes = [];
      app.regional = [];
      app.people = { count: 0, unknown: false, list: [] };
      app.wipedAt = Date.now();
      break;
  }
}
