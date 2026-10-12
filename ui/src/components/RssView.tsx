import { useCallback, useEffect, useMemo, useState } from "react";
import { Download, Pencil, Plus, RefreshCw, Rss, Trash2 } from "lucide-react";
import { api, RssData, RssFeed, RssRule } from "../api";
import { useT } from "../i18n";
import * as fmt from "../format";
import { Check, Dialog, Field, useToast } from "./ui";

export function RssView() {
  const { t, lang } = useT();
  const toast = useToast();
  const [data, setData] = useState<RssData | null>(null);
  const [feedId, setFeedId] = useState<string | null>(null);
  const [editFeed, setEditFeed] = useState<Partial<RssFeed> | null>(null);
  const [editRule, setEditRule] = useState<Partial<RssRule> | null>(null);
  const [showRules, setShowRules] = useState(false);

  const load = useCallback(() => api.rss().then(setData).catch(() => {}), []);
  useEffect(() => {
    load();
    const h = setInterval(load, 5000);
    return () => clearInterval(h);
  }, [load]);

  const items = useMemo(
    () =>
      (data?.items ?? [])
        .filter((i) => !feedId || i.feed_id === feedId)
        .sort((a, b) => (b.published ?? 0) - (a.published ?? 0)),
    [data, feedId],
  );

  const err = (e: unknown) => toast(String((e as Error).message ?? e), true);

  return (
    <div className="rss">
      <div className="feeds">
        <button className="btn" onClick={() => setEditFeed({ interval_min: 30, enabled: true })}>
          <Plus /> {t("addFeed")}
        </button>
        <button className="btn" onClick={() => setShowRules(true)}>
          {t("rules")} ({data?.rules.length ?? 0})
        </button>
        <div className="side-title">{t("feeds")}</div>
        <div className={`side-item ${!feedId ? "active" : ""}`} onClick={() => setFeedId(null)}>
          <Rss /> {t("allFeeds")}
        </div>
        {data?.feeds.map((f) => (
          <div
            key={f.id}
            className={`side-item ${feedId === f.id ? "active" : ""}`}
            onClick={() => setFeedId(f.id)}
            title={f.last_error ?? f.url}
          >
            <Rss style={{ color: f.last_error ? "var(--red)" : undefined }} />
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{f.name}</span>
            <span className="count" style={{ display: "flex", gap: 2 }}>
              <button
                className="icon-btn"
                style={{ width: 22, height: 22 }}
                onClick={(e) => {
                  e.stopPropagation();
                  api.refreshFeed(f.id).then(load).catch(err);
                }}
              >
                <RefreshCw size={12} />
              </button>
              <button
                className="icon-btn"
                style={{ width: 22, height: 22 }}
                onClick={(e) => {
                  e.stopPropagation();
                  setEditFeed(f);
                }}
              >
                <Pencil size={12} />
              </button>
            </span>
          </div>
        ))}
      </div>
      <div className="items">
        {!data?.feeds.length ? (
          <div className="placeholder">{t("noFeeds")}</div>
        ) : !items.length ? (
          <div className="placeholder">{t("noItems")}</div>
        ) : (
          <table className="grid">
            <colgroup>
              <col />
              <col style={{ width: 90 }} />
              <col style={{ width: 130 }} />
              <col style={{ width: 44 }} />
            </colgroup>
            <tbody>
              {items.map((i) => (
                <tr key={i.feed_id + i.guid}>
                  <td title={i.title} className={i.downloaded ? "dim" : ""}>
                    {i.title}
                  </td>
                  <td className="num dim">{i.size ? fmt.bytes(i.size, lang) : ""}</td>
                  <td className="dim">{fmt.date(i.published, lang)}</td>
                  <td>
                    <button
                      className="icon-btn"
                      title={t("download")}
                      disabled={!i.link}
                      onClick={() =>
                        api
                          .downloadItem(i.feed_id, i.guid)
                          .then(() => {
                            toast(t("added_ok"));
                            load();
                          })
                          .catch(err)
                      }
                    >
                      <Download />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {editFeed && (
        <Dialog
          title={editFeed.id ? t("edit") : t("addFeed")}
          onClose={() => setEditFeed(null)}
          footer={
            <>
              {editFeed.id && (
                <button
                  className="btn danger"
                  style={{ marginRight: "auto" }}
                  onClick={() =>
                    api
                      .deleteFeed(editFeed.id!)
                      .then(() => {
                        setEditFeed(null);
                        setFeedId(null);
                        load();
                      })
                      .catch(err)
                  }
                >
                  <Trash2 /> {t("remove")}
                </button>
              )}
              <button className="btn" onClick={() => setEditFeed(null)}>
                {t("cancel")}
              </button>
              <button
                className="btn primary"
                disabled={!editFeed.url}
                onClick={() =>
                  api
                    .saveFeed(editFeed)
                    .then(() => {
                      setEditFeed(null);
                      load();
                    })
                    .catch(err)
                }
              >
                {t("save")}
              </button>
            </>
          }
        >
          <Field label={t("feedUrl")}>
            <input className="input" autoFocus value={editFeed.url ?? ""} onChange={(e) => setEditFeed({ ...editFeed, url: e.target.value })} />
          </Field>
          <div className="grid2">
            <Field label={t("feedName")}>
              <input className="input" value={editFeed.name ?? ""} onChange={(e) => setEditFeed({ ...editFeed, name: e.target.value })} />
            </Field>
            <Field label={t("interval")}>
              <input
                className="input"
                type="number"
                min={5}
                value={editFeed.interval_min ?? 30}
                onChange={(e) => setEditFeed({ ...editFeed, interval_min: Number(e.target.value) })}
              />
            </Field>
          </div>
          <Check checked={editFeed.enabled ?? true} onChange={(v) => setEditFeed({ ...editFeed, enabled: v })}>
            {t("enabled")}
          </Check>
          {editFeed.last_error && <div className="error-text">{editFeed.last_error}</div>}
        </Dialog>
      )}

      {showRules && !editRule && (
        <Dialog
          title={t("rules")}
          onClose={() => setShowRules(false)}
          footer={
            <button className="btn primary" onClick={() => setEditRule({ enabled: true, feed_ids: [] })}>
              <Plus /> {t("addRule")}
            </button>
          }
        >
          {!data?.rules.length && <div className="hint">{t("ruleHint")}</div>}
          {data?.rules.map((r) => (
            <div key={r.id} className="side-item" onClick={() => setEditRule(r)}>
              <input type="checkbox" checked={r.enabled} readOnly />
              <b>{r.name || r.must_contain}</b>
              <span className="dim" style={{ overflow: "hidden", textOverflow: "ellipsis" }}>
                {r.must_contain}
              </span>
            </div>
          ))}
        </Dialog>
      )}

      {editRule && (
        <Dialog
          title={editRule.id ? t("edit") : t("addRule")}
          onClose={() => setEditRule(null)}
          footer={
            <>
              {editRule.id && (
                <button
                  className="btn danger"
                  style={{ marginRight: "auto" }}
                  onClick={() =>
                    api
                      .deleteRule(editRule.id!)
                      .then(() => {
                        setEditRule(null);
                        load();
                      })
                      .catch(err)
                  }
                >
                  <Trash2 /> {t("remove")}
                </button>
              )}
              <button className="btn" onClick={() => setEditRule(null)}>
                {t("cancel")}
              </button>
              <button
                className="btn primary"
                disabled={!editRule.must_contain?.trim()}
                onClick={() =>
                  api
                    .saveRule(editRule)
                    .then(() => {
                      setEditRule(null);
                      load();
                    })
                    .catch(err)
                }
              >
                {t("save")}
              </button>
            </>
          }
        >
          <Field label={t("ruleName")}>
            <input className="input" value={editRule.name ?? ""} onChange={(e) => setEditRule({ ...editRule, name: e.target.value })} />
          </Field>
          <Field label={t("mustContain")} hint={t("ruleHint")}>
            <input
              className="input"
              autoFocus
              value={editRule.must_contain ?? ""}
              onChange={(e) => setEditRule({ ...editRule, must_contain: e.target.value })}
            />
          </Field>
          <Field label={t("mustNotContain")}>
            <input
              className="input"
              value={editRule.must_not_contain ?? ""}
              onChange={(e) => setEditRule({ ...editRule, must_not_contain: e.target.value })}
            />
          </Field>
          <div className="grid2">
            <Field label={t("feeds")}>
              <select
                className="input"
                value={editRule.feed_ids?.[0] ?? ""}
                onChange={(e) => setEditRule({ ...editRule, feed_ids: e.target.value ? [e.target.value] : [] })}
              >
                <option value="">{t("allFeeds")}</option>
                {data?.feeds.map((f) => (
                  <option key={f.id} value={f.id}>
                    {f.name}
                  </option>
                ))}
              </select>
            </Field>
            <Field label={t("category")}>
              <input
                className="input"
                value={editRule.category ?? ""}
                onChange={(e) => setEditRule({ ...editRule, category: e.target.value || null })}
              />
            </Field>
          </div>
          <Field label={t("savePath")}>
            <input
              className="input"
              value={editRule.save_path ?? ""}
              onChange={(e) => setEditRule({ ...editRule, save_path: e.target.value || null })}
            />
          </Field>
          <div className="row-flex" style={{ gap: 20 }}>
            <Check checked={editRule.enabled ?? true} onChange={(v) => setEditRule({ ...editRule, enabled: v })}>
              {t("enabled")}
            </Check>
            <Check checked={editRule.use_regex ?? false} onChange={(v) => setEditRule({ ...editRule, use_regex: v })}>
              {t("useRegex")}
            </Check>
          </div>
        </Dialog>
      )}
    </div>
  );
}
