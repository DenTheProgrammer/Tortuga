import { ReactNode, useEffect, useState } from "react";
import { FolderOpen } from "lucide-react";
import { api, NetInterface, NetworkMode, ScheduleSlot, Settings } from "../api";
import { useT } from "../i18n";
import {
  defaultHandlerStatus,
  detectPlayers,
  isTauri,
  makeDefaultHandler,
  pickExecutable,
  pickFolder,
  PlayerInfo,
  appVersion,
  checkUpdate,
  installUpdate,
  UpdateInfo,
} from "../native";
import { Check, Dialog, Field, useToast } from "./ui";

type Section = "sGeneral" | "sDownloads" | "sSpeed" | "sNetwork" | "sPlayer" | "sVersion";

export function SettingsDialog({
  settings,
  onClose,
  onSaved,
  onUpdate,
}: {
  settings: Settings;
  onClose: () => void;
  onSaved: (s: Settings) => void;
  onUpdate: (u: UpdateInfo | null) => void;
}) {
  const { t } = useT();
  const toast = useToast();
  const [s, setS] = useState<Settings>(structuredClone(settings));
  const [section, setSection] = useState<Section>("sGeneral");
  const [saving, setSaving] = useState(false);

  const set = <K extends keyof Settings>(k: K, v: Settings[K]) => setS((p) => ({ ...p, [k]: v }));

  const save = async () => {
    setSaving(true);
    try {
      const saved = await api.saveSettings(s);
      onSaved(saved);
      onClose();
    } catch (e) {
      toast(String((e as Error).message), true);
    } finally {
      setSaving(false);
    }
  };

  const sections: Section[] = ["sGeneral", "sDownloads", "sSpeed", "sNetwork", "sPlayer", "sVersion"];

  return (
    <Dialog
      title={t("settings")}
      onClose={onClose}
      wide
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {t("cancel")}
          </button>
          {section !== "sVersion" && <button className="btn primary" onClick={save} disabled={saving}>
            {t("save")}
          </button>}
        </>
      }
    >
      <div className="settings">
        <nav>
          {sections.map((k) => (
            <div key={k} className={`side-item ${section === k ? "active" : ""}`} onClick={() => setSection(k)}>
              {t(k)}
            </div>
          ))}
        </nav>
        <div className="pane">
          {section === "sGeneral" && <General s={s} set={set} />}
          {section === "sDownloads" && <Downloads s={s} set={set} />}
          {section === "sSpeed" && <Speed s={s} set={set} />}
          {section === "sNetwork" && <Network s={s} set={set} />}
          {section === "sPlayer" && <Player s={s} set={set} />}
          {section === "sVersion" && <Version onUpdate={onUpdate} />}
        </div>
      </div>
    </Dialog>
  );
}

function Version({ onUpdate }: { onUpdate: (u: UpdateInfo | null) => void }) {
  const { t } = useT();
  const [version, setVersion] = useState<string | null>(null);
  const [status, setStatus] = useState<"idle" | "checking" | "current" | "updating">("idle");
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    appVersion().then((v) => active && setVersion(v)).catch((e) => active && setError(String(e)));
    return () => { active = false; };
  }, []);
  const check = async () => {
    setStatus("checking");
    setError(null);
    try {
      const found = await checkUpdate();
      setUpdate(found);
      setStatus(found ? "idle" : "current");
      onUpdate(found);
    } catch (e) {
      setError(String(e));
      setStatus("idle");
    }
  };
  return (
    <>
      <Field label={t("installedVersion")}><strong>Tortuga {version ?? "…"}</strong></Field>
      <div>
        <button className="btn" onClick={check} disabled={!isTauri || status === "checking" || status === "updating"}>
          {status === "checking" ? t("checkingUpdates") : t("checkUpdates")}
        </button>
      </div>
      <div className="hint">{t(isTauri ? "autoUpdateHint" : "updatesDesktopOnly")}</div>
      {status === "current" && <div role="status" className="note">{t("upToDate")}</div>}
      {update && <div className="note">
        {t("updateAvailable")} <b>{update.version}</b>
        <div style={{ marginTop: 12 }}><button className="btn primary" disabled={status === "updating"} onClick={async () => {
          setStatus("updating");
          setError(null);
          try { await installUpdate(); }
          catch (e) { setError(String(e)); setStatus("idle"); }
        }}>{status === "updating" ? t("updating") : t("updateNow")}</button></div>
      </div>}
      {error && <div role="alert" className="note" style={{ color: "var(--red)" }}>{t("updateCheckError")} {error}</div>}
    </>
  );
}

type P = { s: Settings; set: <K extends keyof Settings>(k: K, v: Settings[K]) => void };

function General({ s, set }: P) {
  const { t } = useT();
  const [handler, setHandler] = useState<{ supported: boolean; is_default: boolean } | null>(null);
  useEffect(() => {
    defaultHandlerStatus().then(setHandler).catch(() => {});
  }, []);
  return (
    <>
      <div className="grid2">
        <Field label={t("language")}>
          <select className="input" value={s.language} onChange={(e) => set("language", e.target.value)}>
            <option value="ru">Русский</option>
            <option value="en">English</option>
          </select>
        </Field>
        <Field label={t("themeLabel")}>
          <select className="input" value={s.theme} onChange={(e) => set("theme", e.target.value)}>
            <option value="dark">{t("themeDark")}</option>
            <option value="light">{t("themeLight")}</option>
            <option value="system">{t("themeSystem")}</option>
          </select>
        </Field>
      </div>
      {isTauri && (
        <>
          <Check checked={s.close_to_tray} onChange={(v) => set("close_to_tray", v)}>
            {t("closeToTray")}
          </Check>
          <Check checked={s.start_minimized} onChange={(v) => set("start_minimized", v)}>
            {t("startMinimized")}
          </Check>
          <Check checked={s.notify_on_complete} onChange={(v) => set("notify_on_complete", v)}>
            {t("notifyComplete")}
          </Check>
        </>
      )}
      {handler?.supported && (
        <Field label={t("defaultHandler")}>
          {handler.is_default ? (
            <div className="note">✓ {t("isDefault")}</div>
          ) : (
            <div>
              <button
                className="btn"
                onClick={async () => {
                  await makeDefaultHandler();
                  setTimeout(() => defaultHandlerStatus().then(setHandler), 1500);
                }}
              >
                {t("makeDefault")}
              </button>
            </div>
          )}
        </Field>
      )}
    </>
  );
}

function Downloads({ s, set }: P) {
  const { t } = useT();
  return (
    <>
      <Field label={t("downloadDir")}>
        <div className="row-flex">
          <input className="input" value={s.download_dir} onChange={(e) => set("download_dir", e.target.value)} />
          {isTauri && (
            <button
              className="icon-btn"
              title={t("browse")}
              onClick={async () => set("download_dir", (await pickFolder(s.download_dir)) ?? s.download_dir)}
            >
              <FolderOpen />
            </button>
          )}
        </div>
      </Field>
      <Check checked={s.start_paused} onChange={(v) => set("start_paused", v)}>
        {t("startPaused")}
      </Check>
      <Check checked={s.default_sequential} onChange={(v) => set("default_sequential", v)}>
        {t("defaultSequential")}
      </Check>
      <Field label={t("ratioLimit")}>
        <input
          className="input"
          type="number"
          min={0}
          step={0.1}
          style={{ width: 140 }}
          placeholder={t("noLimit")}
          value={s.seed_ratio_limit ?? ""}
          onChange={(e) => set("seed_ratio_limit", e.target.value === "" ? null : Number(e.target.value))}
        />
      </Field>
    </>
  );
}

function NumField({ label, value, onChange }: { label: ReactNode; value: number; onChange: (v: number) => void }) {
  return (
    <Field label={label}>
      <input
        className="input"
        type="number"
        min={0}
        value={value}
        onChange={(e) => onChange(Math.max(0, Math.floor(Number(e.target.value) || 0)))}
      />
    </Field>
  );
}

function Speed({ s, set }: P) {
  const { t } = useT();
  const days = t("days").split(",");
  const cycle: Record<ScheduleSlot, ScheduleSlot> = { normal: "alt", alt: "off", off: "normal" };
  const toggle = (d: number, h: number) => {
    const grid = s.scheduler.grid.map((row) => [...row]);
    grid[d][h] = cycle[grid[d][h]];
    set("scheduler", { ...s.scheduler, grid });
  };
  return (
    <>
      <div className="grid2">
        <NumField label={t("limitDown")} value={s.download_limit} onChange={(v) => set("download_limit", v)} />
        <NumField label={t("limitUp")} value={s.upload_limit} onChange={(v) => set("upload_limit", v)} />
      </div>
      <div className="side-title" style={{ padding: "4px 0 0" }}>
        {t("altLimits")}
      </div>
      <div className="grid2">
        <NumField label={t("limitDown")} value={s.alt_download_limit} onChange={(v) => set("alt_download_limit", v)} />
        <NumField label={t("limitUp")} value={s.alt_upload_limit} onChange={(v) => set("alt_upload_limit", v)} />
      </div>
      <div className="hint">{t("zeroUnlimited")}</div>
      <Check checked={s.scheduler.enabled} onChange={(v) => set("scheduler", { ...s.scheduler, enabled: v })}>
        {t("scheduler")}
      </Check>
      {s.scheduler.enabled && (
        <>
          <div className="sched">
            <div />
            {Array.from({ length: 24 }, (_, h) => (
              <div key={h} style={{ textAlign: "center" }}>
                {h % 3 === 0 ? h : ""}
              </div>
            ))}
            {s.scheduler.grid.map((row, d) => (
              <div key={d} style={{ display: "contents" }}>
                <div>{days[d]}</div>
                {row.map((slot, h) => (
                  <div
                    key={h}
                    className={`cell ${slot}`}
                    title={`${days[d]} ${h}:00 — ${t(`slot_${slot}` as never)}`}
                    onClick={() => toggle(d, h)}
                  />
                ))}
              </div>
            ))}
          </div>
          <div className="legend">
            <span>
              <i style={{ background: "var(--bar-bg)" }} />
              {t("slot_normal")}
            </span>
            <span>
              <i style={{ background: "var(--accent)" }} />
              {t("slot_alt")}
            </span>
            <span>
              <i style={{ background: "var(--red)" }} />
              {t("slot_off")}
            </span>
          </div>
          <div className="hint">{t("schedulerHint")}</div>
        </>
      )}
    </>
  );
}

function Network({ s, set }: P) {
  const { t } = useT();
  const [ifaces, setIfaces] = useState<NetInterface[]>([]);
  useEffect(() => {
    api
      .network()
      .then((n) => setIfaces(n.snapshot?.interfaces ?? []))
      .catch(() => {});
  }, []);
  const modes: NetworkMode[] = ["auto", "system", "direct", "vpn_only"];
  const setNet = (patch: Partial<Settings["network"]>) => set("network", { ...s.network, ...patch });
  const ifaceSelect = (value: string | null, onChange: (v: string | null) => void, vpn: boolean) => (
    <Field label={t("interface")}>
      <select className="input" value={value ?? ""} onChange={(e) => onChange(e.target.value || null)}>
        <option value="">{t("autoPick")}</option>
        {ifaces
          .filter((i) => i.is_vpn === vpn)
          .map((i) => (
            <option key={i.bind_name} value={i.bind_name}>
              {i.display_name} — {i.ipv4.join(", ")}
            </option>
          ))}
      </select>
    </Field>
  );
  return (
    <>
      <div className="field">
        <span>{t("networkMode")}</span>
        {modes.map((m) => (
          <label key={m} className={`radio-card ${s.network.mode === m ? "on" : ""}`}>
            <input type="radio" checked={s.network.mode === m} onChange={() => setNet({ mode: m })} />
            <div>
              <b>{t(`nm_${m}` as never)}</b>
              <div className="hint">{t(`nm_${m}_hint` as never)}</div>
            </div>
          </label>
        ))}
      </div>
      {s.network.mode === "direct" && ifaceSelect(s.network.direct_interface, (v) => setNet({ direct_interface: v }), false)}
      {s.network.mode === "vpn_only" && ifaceSelect(s.network.vpn_interface, (v) => setNet({ vpn_interface: v }), true)}
      {(s.network.mode === "direct" || s.network.mode === "auto") && <div className="note">{t("vpnKillSwitchNote")}</div>}
      <div className="grid2">
        <NumField label={t("listenPort")} value={s.listen_port} onChange={(v) => set("listen_port", Math.min(65535, v))} />
        <NumField label={t("peerLimit")} value={s.peer_limit} onChange={(v) => set("peer_limit", Math.max(1, v))} />
      </div>
      <Check checked={s.upnp} onChange={(v) => set("upnp", v)}>
        {t("upnp")}
      </Check>
      <Check checked={s.dht} onChange={(v) => set("dht", v)}>
        {t("dht")}
      </Check>
      <Check checked={s.lsd} onChange={(v) => set("lsd", v)}>
        {t("lsd")}
      </Check>
      <Check checked={s.utp} onChange={(v) => set("utp", v)}>
        {t("utp")}
      </Check>
      <Field label={t("proxy")}>
        <input
          className="input"
          placeholder="socks5://user:pass@host:1080"
          value={s.proxy_url ?? ""}
          onChange={(e) => set("proxy_url", e.target.value || null)}
        />
      </Field>
    </>
  );
}

function Player({ s, set }: P) {
  const { t } = useT();
  const [players, setPlayers] = useState<PlayerInfo[]>([]);
  useEffect(() => {
    detectPlayers().then(setPlayers).catch(() => {});
  }, []);
  const options: { id: string; label: string; found?: boolean }[] = [
    { id: "builtin", label: t("pl_builtin") },
    ...(isTauri ? [{ id: "system", label: t("pl_system") }] : []),
    ...players.map((p) => ({ id: p.id, label: p.name, found: !!p.path })),
    ...(isTauri ? [{ id: "custom", label: t("pl_custom") }] : []),
  ];
  return (
    <>
      <div className="field">
        <span>{t("player")}</span>
        {options.map((o) => (
          <label key={o.id} className={`radio-card ${s.player === o.id ? "on" : ""}`}>
            <input type="radio" checked={s.player === o.id} onChange={() => set("player", o.id)} />
            <div>
              <b>{o.label}</b>
              {o.found === false && <span className="hint"> — {t("notInstalled")}</span>}
            </div>
          </label>
        ))}
      </div>
      {s.player === "custom" && (
        <div className="row-flex">
          <input className="input" value={s.player_path ?? ""} onChange={(e) => set("player_path", e.target.value || null)} />
          <button className="btn" onClick={async () => set("player_path", (await pickExecutable()) ?? s.player_path)}>
            {t("browse")}
          </button>
        </div>
      )}
      <div className="note">{t("playerHint")}</div>
    </>
  );
}
