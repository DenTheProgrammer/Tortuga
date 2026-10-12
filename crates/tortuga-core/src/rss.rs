//! RSS / Atom feeds with auto-download rules.

use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};

use anyhow::Context;
use parking_lot::Mutex;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{
    engine::{AddRequest, Engine, Event},
    util::{now_ts, random_token, write_json_atomic},
};

const MAX_ITEMS_PER_FEED: usize = 300;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Feed {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub interval_min: u32,
    pub last_checked: Option<i64>,
    pub last_error: Option<String>,
}

impl Default for Feed {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            url: String::new(),
            enabled: true,
            interval_min: 30,
            last_checked: None,
            last_error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Feeds the rule applies to; empty = all feeds.
    pub feed_ids: Vec<String>,
    /// Words that must all appear (or a regex when `use_regex`).
    pub must_contain: String,
    pub must_not_contain: String,
    pub use_regex: bool,
    pub category: Option<String>,
    pub save_path: Option<String>,
    pub start_paused: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            feed_ids: Vec::new(),
            must_contain: String::new(),
            must_not_contain: String::new(),
            use_regex: false,
            category: None,
            save_path: None,
            start_paused: false,
        }
    }
}

impl Rule {
    fn text_matches(pattern: &str, regex: bool, title: &str) -> Option<bool> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }
        if regex {
            return RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .ok()
                .map(|r| r.is_match(title));
        }
        let title = title.to_lowercase();
        // "a b | c d" = (a AND b) OR (c AND d)
        Some(pattern.split('|').any(|alt| {
            let words: Vec<String> = alt.split_whitespace().map(|w| w.to_lowercase()).collect();
            !words.is_empty() && words.iter().all(|w| title.contains(w.as_str()))
        }))
    }

    pub fn matches(&self, feed_id: &str, title: &str) -> bool {
        if !self.enabled {
            return false;
        }
        if !self.feed_ids.is_empty() && !self.feed_ids.iter().any(|f| f == feed_id) {
            return false;
        }
        let Some(true) = Self::text_matches(&self.must_contain, self.use_regex, title) else {
            return false;
        };
        !matches!(
            Self::text_matches(&self.must_not_contain, self.use_regex, title),
            Some(true)
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub feed_id: String,
    pub guid: String,
    pub title: String,
    pub link: Option<String>,
    pub published: Option<i64>,
    pub size: Option<u64>,
    pub downloaded: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RssData {
    pub feeds: Vec<Feed>,
    pub rules: Vec<Rule>,
    pub items: Vec<Item>,
}

pub struct RssStore {
    path: PathBuf,
    data: Mutex<RssData>,
}

static STORE: Mutex<Option<Arc<RssStore>>> = Mutex::new(None);

pub fn store(engine: &Engine) -> Arc<RssStore> {
    STORE
        .lock()
        .get_or_insert_with(|| {
            let path = engine.data_dir.join("rss.json");
            let data = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            Arc::new(RssStore {
                path,
                data: Mutex::new(data),
            })
        })
        .clone()
}

impl RssStore {
    pub fn snapshot(&self) -> RssData {
        self.data.lock().clone()
    }

    fn save(&self) {
        let d = self.data.lock().clone();
        if let Err(e) = write_json_atomic(&self.path, &d) {
            warn!("error saving rss: {e:#}");
        }
    }

    pub fn upsert_feed(&self, mut feed: Feed) -> anyhow::Result<Feed> {
        url::Url::parse(feed.url.trim()).context("invalid feed URL")?;
        feed.url = feed.url.trim().to_owned();
        feed.interval_min = feed.interval_min.max(5);
        {
            let mut d = self.data.lock();
            if feed.id.is_empty() {
                feed.id = random_token()[..12].to_owned();
                if feed.name.trim().is_empty() {
                    feed.name = feed.url.clone();
                }
                d.feeds.push(feed.clone());
            } else {
                let f = d
                    .feeds
                    .iter_mut()
                    .find(|f| f.id == feed.id)
                    .context("feed not found")?;
                f.name = feed.name.clone();
                f.url = feed.url.clone();
                f.enabled = feed.enabled;
                f.interval_min = feed.interval_min;
                feed = f.clone();
            }
        }
        self.save();
        Ok(feed)
    }

    pub fn delete_feed(&self, id: &str) {
        {
            let mut d = self.data.lock();
            d.feeds.retain(|f| f.id != id);
            d.items.retain(|i| i.feed_id != id);
            for r in d.rules.iter_mut() {
                r.feed_ids.retain(|f| f != id);
            }
        }
        self.save();
    }

    pub fn upsert_rule(&self, mut rule: Rule) -> anyhow::Result<Rule> {
        if rule.use_regex {
            RegexBuilder::new(&rule.must_contain)
                .build()
                .context("invalid regex in 'must contain'")?;
            RegexBuilder::new(&rule.must_not_contain)
                .build()
                .context("invalid regex in 'must not contain'")?;
        }
        {
            let mut d = self.data.lock();
            if rule.id.is_empty() {
                rule.id = random_token()[..12].to_owned();
                d.rules.push(rule.clone());
            } else {
                let r = d
                    .rules
                    .iter_mut()
                    .find(|r| r.id == rule.id)
                    .context("rule not found")?;
                *r = rule.clone();
            }
        }
        self.save();
        Ok(rule)
    }

    pub fn delete_rule(&self, id: &str) {
        self.data.lock().rules.retain(|r| r.id != id);
        self.save();
    }
}

fn parse_feed(feed_id: &str, bytes: &[u8]) -> anyhow::Result<Vec<Item>> {
    let parsed = feed_rs::parser::parse(bytes).context("can't parse feed")?;
    Ok(parsed
        .entries
        .into_iter()
        .map(|e| {
            let title = e
                .title
                .as_ref()
                .map(|t| t.content.trim().to_owned())
                .unwrap_or_default();
            let mut candidates: Vec<(String, Option<u64>)> = Vec::new();
            for m in &e.media {
                for c in &m.content {
                    if let Some(u) = &c.url {
                        candidates.push((u.to_string(), c.size));
                    }
                }
            }
            for l in &e.links {
                candidates.push((l.href.clone(), l.length));
            }
            let is_torrent = |u: &str| {
                u.starts_with("magnet:")
                    || u.to_lowercase()
                        .split('?')
                        .next()
                        .unwrap_or("")
                        .ends_with(".torrent")
            };
            let best = candidates
                .iter()
                .find(|(u, _)| u.starts_with("magnet:"))
                .or_else(|| candidates.iter().find(|(u, _)| is_torrent(u)))
                .or_else(|| candidates.first())
                .cloned();
            Item {
                feed_id: feed_id.to_owned(),
                guid: if e.id.is_empty() {
                    title.clone()
                } else {
                    e.id.clone()
                },
                link: best.as_ref().map(|b| b.0.clone()),
                size: best.and_then(|b| b.1),
                published: e.published.or(e.updated).map(|d| d.timestamp()),
                title,
                downloaded: false,
            }
        })
        .collect())
}

async fn fetch(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.bytes().await?.to_vec())
}

pub async fn refresh_feed(engine: &Arc<Engine>, feed_id: &str) -> anyhow::Result<()> {
    let store = store(engine);
    let feed = store
        .snapshot()
        .feeds
        .into_iter()
        .find(|f| f.id == feed_id)
        .context("feed not found")?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("Tortuga/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let res = async { parse_feed(&feed.id, &fetch(&client, &feed.url).await?) }.await;

    let mut to_download: Vec<(Item, Rule)> = Vec::new();
    {
        let mut d = store.data.lock();
        let rules = d.rules.clone();
        let known: HashSet<String> = d
            .items
            .iter()
            .filter(|i| i.feed_id == feed.id)
            .map(|i| i.guid.clone())
            .collect();
        if let Some(f) = d.feeds.iter_mut().find(|f| f.id == feed.id) {
            f.last_checked = Some(now_ts());
            f.last_error = res.as_ref().err().map(|e| format!("{e:#}"));
        }
        if let Ok(items) = res.as_ref() {
            for item in items.iter().rev() {
                if known.contains(&item.guid) {
                    continue;
                }
                let mut item = item.clone();
                if item.link.is_some()
                    && let Some(rule) = rules.iter().find(|r| r.matches(&feed.id, &item.title))
                {
                    item.downloaded = true;
                    to_download.push((item.clone(), rule.clone()));
                }
                d.items.push(item);
            }
            // Keep the newest items per feed.
            let count = d.items.iter().filter(|i| i.feed_id == feed.id).count();
            if count > MAX_ITEMS_PER_FEED {
                let mut drop = count - MAX_ITEMS_PER_FEED;
                d.items.retain(|i| {
                    if drop > 0 && i.feed_id == feed.id {
                        drop -= 1;
                        false
                    } else {
                        true
                    }
                });
            }
        }
    }
    store.save();

    for (item, rule) in to_download {
        info!(title = %item.title, rule = %rule.name, "rss: auto-downloading");
        let req = AddRequest {
            source: item.link.clone(),
            save_path: rule.save_path.clone(),
            paused: Some(rule.start_paused),
            category: rule.category.clone(),
            ..Default::default()
        };
        match engine.add(req).await {
            Ok(_) => engine.emit(Event::RssDownloaded {
                title: item.title.clone(),
                feed: feed.name.clone(),
            }),
            Err(e) => engine.emit(Event::Error {
                id: None,
                message: format!("RSS «{}»: {e:#}", item.title),
            }),
        }
    }
    res.map(|_| ())
}

pub async fn download_item(engine: &Arc<Engine>, feed_id: &str, guid: &str) -> anyhow::Result<()> {
    let store = store(engine);
    let link = {
        let mut d = store.data.lock();
        let item = d
            .items
            .iter_mut()
            .find(|i| i.feed_id == feed_id && i.guid == guid)
            .context("item not found")?;
        item.downloaded = true;
        item.link.clone().context("item has no torrent link")?
    };
    store.save();
    engine
        .add(AddRequest {
            source: Some(link),
            ..Default::default()
        })
        .await?;
    Ok(())
}

pub(crate) fn spawn(engine: Arc<Engine>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let now = now_ts();
            let due: Vec<String> = store(&engine)
                .snapshot()
                .feeds
                .into_iter()
                .filter(|f| f.enabled)
                .filter(|f| {
                    f.last_checked
                        .is_none_or(|t| now - t >= f.interval_min as i64 * 60)
                })
                .map(|f| f.id)
                .collect();
            for id in due {
                if let Err(e) = refresh_feed(&engine, &id).await {
                    warn!(feed = %id, "rss refresh failed: {e:#}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_word_matching() {
        let r = Rule {
            must_contain: "show 1080p | movie 4k".into(),
            must_not_contain: "cam".into(),
            ..Default::default()
        };
        assert!(r.matches("f", "The Show S01E02 1080p WEB"));
        assert!(r.matches("f", "Some Movie 2024 4K HDR"));
        assert!(!r.matches("f", "The Show S01E02 720p"));
        assert!(!r.matches("f", "The Show 1080p CAM"));
    }

    #[test]
    fn rule_regex_and_feed_filter() {
        let r = Rule {
            must_contain: r"s\d\de\d\d".into(),
            use_regex: true,
            feed_ids: vec!["a".into()],
            ..Default::default()
        };
        assert!(r.matches("a", "Show S01E05"));
        assert!(!r.matches("b", "Show S01E05"));
    }

    #[test]
    fn parses_rss_with_enclosure() {
        let xml = br#"<?xml version="1.0"?><rss version="2.0"><channel><title>t</title>
            <item><title>Ep 1</title><guid>g1</guid>
            <enclosure url="https://example.com/ep1.torrent" length="123" type="application/x-bittorrent"/></item>
            <item><title>Ep 2</title><link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link></item>
            </channel></rss>"#;
        let items = parse_feed("f", xml).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].link.as_deref(),
            Some("https://example.com/ep1.torrent")
        );
        assert!(items[1].link.as_deref().unwrap().starts_with("magnet:"));
    }
}
