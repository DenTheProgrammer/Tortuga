import { useState } from "react";
import { File, Folder } from "lucide-react";
import { api, CreateResponse } from "../api";
import { useT } from "../i18n";
import { copyText, isTauri, pickAnyPath, pickSavePath } from "../native";
import { Check, Dialog, Field, useToast } from "./ui";

const PIECE_SIZES = [0, 256, 512, 1024, 2048, 4096, 8192, 16384];

export function CreateDialog({ onClose }: { onClose: () => void }) {
  const { t } = useT();
  const toast = useToast();
  const [path, setPath] = useState("");
  const [trackers, setTrackers] = useState("");
  const [comment, setComment] = useState("");
  const [piece, setPiece] = useState(0);
  const [seed, setSeed] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<CreateResponse | null>(null);

  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      const name = path.split(/[\\/]/).filter(Boolean).pop() ?? "torrent";
      const saveTo = isTauri ? await pickSavePath(`${name}.torrent`) : null;
      const r = await api.create({
        path,
        trackers: trackers.split("\n"),
        comment: comment || undefined,
        piece_length: piece ? piece * 1024 : undefined,
        seed,
        save_to: saveTo ?? undefined,
      });
      if (!isTauri) {
        // Browser: download the file.
        const a = document.createElement("a");
        a.href = `data:application/x-bittorrent;base64,${r.torrent_base64}`;
        a.download = `${r.name}.torrent`;
        a.click();
      }
      setResult(r);
      toast(t("createdOk"));
    } catch (e) {
      setError(String((e as Error).message ?? e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={t("create")}
      onClose={onClose}
      footer={
        result ? (
          <button className="btn primary" onClick={onClose}>
            {t("close")}
          </button>
        ) : (
          <>
            <button className="btn" onClick={onClose}>
              {t("cancel")}
            </button>
            <button className="btn primary" disabled={busy || !path.trim()} onClick={create}>
              {busy ? t("creating") : t("create")}
            </button>
          </>
        )
      }
    >
      {result ? (
        <>
          <b>{result.name}</b>
          {result.saved_to && <div className="hint">{result.saved_to}</div>}
          <div className="row-flex">
            <input className="input" readOnly value={result.magnet} />
            <button
              className="btn"
              onClick={() => {
                copyText(result.magnet);
                toast(t("copied"));
              }}
            >
              {t("copyMagnet")}
            </button>
          </div>
        </>
      ) : (
        <>
          <Field label={t("createSource")}>
            <div className="row-flex">
              <input className="input" value={path} onChange={(e) => setPath(e.target.value)} />
              {isTauri && (
                <>
                  <button className="btn" onClick={async () => setPath((await pickAnyPath(false)) ?? path)}>
                    <File /> {t("file")}
                  </button>
                  <button className="btn" onClick={async () => setPath((await pickAnyPath(true)) ?? path)}>
                    <Folder /> {t("folder")}
                  </button>
                </>
              )}
            </div>
          </Field>
          <Field label={t("trackersOnePerLine")}>
            <textarea className="input" rows={4} value={trackers} onChange={(e) => setTrackers(e.target.value)} />
          </Field>
          <div className="grid2">
            <Field label={t("comment")}>
              <input className="input" value={comment} onChange={(e) => setComment(e.target.value)} />
            </Field>
            <Field label={t("pieceSize")}>
              <select className="input" value={piece} onChange={(e) => setPiece(Number(e.target.value))}>
                {PIECE_SIZES.map((p) => (
                  <option key={p} value={p}>
                    {p === 0 ? t("auto") : p >= 1024 ? `${p / 1024} MB` : `${p} KB`}
                  </option>
                ))}
              </select>
            </Field>
          </div>
          <Check checked={seed} onChange={setSeed}>
            {t("startSeeding")}
          </Check>
          {error && <div className="error-text">{error}</div>}
        </>
      )}
    </Dialog>
  );
}
