import type { TorrentSummary } from "./api";

export const canStart = (t: TorrentSummary) =>
  t.state !== "metadata" && (t.user_paused || t.state === "paused" || t.state === "completed" || t.state === "error");

export const canPause = (t: TorrentSummary) =>
  !t.user_paused && ["downloading", "stalled", "checking", "seeding", "queued"].includes(t.state);
