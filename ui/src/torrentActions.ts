import type { TorrentSummary } from "./api";

export const canStart = (t: TorrentSummary) =>
  !["metadata", "error"].includes(t.state) && (t.user_paused || t.state === "paused" || t.state === "completed");

export const canPause = (t: TorrentSummary) =>
  !t.user_paused && ["downloading", "stalled", "checking", "seeding", "queued"].includes(t.state);
