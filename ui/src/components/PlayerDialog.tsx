import { useState } from "react";
import { Copy, ExternalLink } from "lucide-react";
import { useT } from "../i18n";
import { copyText, isTauri } from "../native";
import { Dialog, useToast } from "./ui";

export function PlayerDialog({
  url,
  name,
  onExternal,
  onClose,
}: {
  url: string;
  name: string;
  onExternal: () => void;
  onClose: () => void;
}) {
  const { t } = useT();
  const toast = useToast();
  const [failed, setFailed] = useState(false);
  return (
    <Dialog
      title={name}
      onClose={onClose}
      className="player"
      footer={
        <>
          <div className="hint" style={{ marginRight: "auto", color: failed ? "var(--red)" : undefined }}>
            {t("codecHint")}
          </div>
          <button
            className="btn"
            onClick={() => {
              copyText(url);
              toast(t("copied"));
            }}
          >
            <Copy /> {t("copyStreamLink")}
          </button>
          {isTauri && (
            <button className="btn primary" onClick={onExternal}>
              <ExternalLink /> {t("externalPlayer")}
            </button>
          )}
        </>
      }
    >
      <video src={url} controls autoPlay onError={() => setFailed(true)} />
    </Dialog>
  );
}
