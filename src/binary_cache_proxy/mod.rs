use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

use ahash::HashMap;
use arc_swap::ArcSwap;
use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use dedup::{Deduper, Unique};
use futures::stream::FuturesUnordered;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    config::Config,
    discover::{Discover, Event},
    signing::KeyStore,
    util::DynamicTimer,
};

mod dedup;

#[derive(serde::Deserialize, Clone, Debug, Copy)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ErrorStrategy {
    Remove,
    Timeout {
        #[serde(rename = "for", with = "humantime_serde")]
        timeout: Duration,
    },
}

async fn cache_info(server: State<Arc<Server>>) -> String {
    format!(
        "\
StoreDir: /nix/store
WantMassQuery: 1
Priority: {}
",
        server.config.priority
    )
}

#[derive(Default, Clone)]
struct CacheData {
    priority_map: BTreeMap<u8, BTreeSet<Unique<Url>>>,
    caches: BTreeMap<Unique<Url>, CacheMeta>,
}

#[derive(Clone, Copy)]
struct CacheMeta {
    priority: u8,
    error_strategy: ErrorStrategy,
    timed_out_until: Option<Instant>,
}

impl CacheData {
    fn iter_priorities(&self) -> impl Iterator<Item = (&u8, &BTreeSet<Unique<Url>>)> {
        self.priority_map.iter()
    }

    fn contains(&self, cache: &Unique<Url>) -> bool {
        self.caches.contains_key(cache)
    }

    fn insert_cache(&mut self, priority: u8, error_strategy: ErrorStrategy, cache: Unique<Url>) {
        if let Some(meta) = self.caches.get_mut(&cache) {
            if meta.priority != priority {
                let bucket = self
                    .priority_map
                    .get_mut(&meta.priority)
                    .expect("Invalid state");
                bucket.remove(&cache);
                self.priority_map.entry(priority).or_default().insert(cache);
                meta.priority = priority;
            }
        } else {
            self.caches.insert(
                cache.clone(),
                CacheMeta {
                    priority,
                    error_strategy,
                    timed_out_until: None,
                },
            );
            self.priority_map.entry(priority).or_default().insert(cache);
        }
    }

    fn remove(&mut self, url: &Unique<Url>) -> bool {
        if let Some(meta) = self.caches.remove(url) {
            let bucket = self
                .priority_map
                .get_mut(&meta.priority)
                .expect("Invalid state");
            bucket.remove(url);
            if bucket.is_empty() {
                self.priority_map.remove(&meta.priority);
            }
            true
        } else {
            false
        }
    }

    fn get_meta(&self, url: &Unique<Url>) -> Option<&CacheMeta> {
        self.caches.get(url)
    }

    fn get_meta_mut(&mut self, url: &Unique<Url>) -> Option<&mut CacheMeta> {
        self.caches.get_mut(url)
    }
}

#[axum::debug_handler]
#[tracing::instrument(skip(server))]
async fn proxy(server: State<Arc<Server>>, req: Request) -> impl IntoResponse {
    let caches = server.caches.load();
    let path = req.uri().path();
    let query = req.uri().query();

    // TODO: maybe make this a Vec<bool> and turn urls from BTreeSet into Vec
    // so no need for BTreeSet
    // also gets rid of cache cloning
    let mut waiting_for = BTreeSet::default();
    for (priority, urls) in caches.iter_priorities() {
        tracing::debug!(priority = priority, caches = ?urls, "Trying");
        waiting_for.clear();
        waiting_for.extend(urls);

        let mut requests = FuturesUnordered::new();
        let now = Instant::now();
        for cache in urls {
            let meta = caches.get_meta(cache).unwrap();

            if let Some(timed_out_until) = meta.timed_out_until {
                if timed_out_until > now {
                    continue;
                }
            }

            requests.push({
                let cache = cache.clone();
                let url = {
                    let mut url = Url::clone(&cache);
                    url.set_path(path);
                    url.set_query(query);
                    url
                };
                let client = server.client.clone();
                let headers = req.headers().clone();
                async move {
                    let ret = client.get(url).headers(headers).send().await;
                    (ret, cache)
                }
            });
        }

        let timeout_duration = server
            .config
            .priority_config
            .get(priority)
            .map(|entry| entry.timeout)
            .unwrap_or(Duration::from_secs(5));
        let timeout = tokio::time::sleep(timeout_duration);
        tokio::pin!(timeout);

        let start = Instant::now();
        let (first_hit, timed_out) = loop {
            tokio::select! {
                _ = &mut timeout => {
                    break (None, true);
                }
                Some((resp, cache)) = requests.next() => {
                    waiting_for.remove(&cache);
                     match resp {
                        Ok(resp) if resp.status() == StatusCode::OK => {
                            break (Some(resp), false);
                        }
                        Ok(resp) => {
                            tracing::debug!(url = %resp.url(), status = %resp.status(), "Got bad status");
                        },
                        Err(error) => {
                            tracing::error!(cache = %&*cache, %error, "Error fetching from cache");
                            server.manage_tx.send(cache).await.unwrap();
                        }
                    };
                }
                else => break (None, false),
            }
        };

        if let Some(resp) = first_hit {
            tracing::debug!(
                hit = %resp.url(),
                "Took {:?} to find first hit",
                start.elapsed()
            );
            return Response::from(resp);
        } else if timed_out {
            for &cache in &waiting_for {
                tracing::error!(cache = %&**cache, deadline = ?timeout_duration, "Cache response exceeded deadline");
                server.manage_tx.send(cache.clone()).await.unwrap();
            }
        }
    }

    // maybe do this if this is used as an exclusive binary cache
    // tracing::warn!("Can't find {:?}", req);
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body("".into())
        .unwrap()
}

pub fn routes() -> axum::Router<Arc<Server>> {
    Router::new()
        .route("/nix-cache-info", get(cache_info))
        .route("/*path", get(proxy))
}

pub struct Server {
    caches: ArcSwap<CacheData>,
    client: reqwest::Client,
    config: Config,
    manage_tx: mpsc::Sender<Unique<Url>>,
}

impl Server {
    pub async fn new(
        set: &mut tokio::task::JoinSet<Result<(), eyre::Error>>,
        config: Config,
        keystore: KeyStore,
        token: CancellationToken,
    ) -> Result<Arc<Self>, eyre::Error> {
        let mut deduper: Deduper<Url> = Deduper::new();
        let mut caches = CacheData::default();
        for host in &config.binary_caches {
            let url = deduper.get_or_insert(host.url.clone());
            caches.insert_cache(host.priority, host.error_strategy, url);
        }

        let ugly_hack = if config.local_binary_caches.is_some() {
            Some(Discover::run(set, config.clone(), keystore, token).await?)
        } else {
            None
        };

        let (manage_tx, mut manage_rx) = mpsc::channel(1);

        let ret = Arc::new(Self {
            caches: ArcSwap::new(Arc::new(caches)),
            client: reqwest::Client::new(),
            config,
            manage_tx,
        });

        tokio::spawn({
            let server = ret.clone();
            async move {
                let Some(local_cache_config) = &server.config.local_binary_caches else {
                    return;
                };

                let Some((discover, mut events)) = ugly_hack else {
                    return;
                };

                let mut interval = tokio::time::interval(local_cache_config.discovery_refresh_time);
                let (batch_timer, mut batch_timeout) = DynamicTimer::new();
                let mut batch: HashMap<Unique<Url>, std::net::IpAddr> = HashMap::default();
                let mut last_adv: Option<Instant> = None;
                let mut local_caches = HashMap::default();

                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            match last_adv {
                                Some(last_adv) if last_adv.elapsed() < local_cache_config.discovery_refresh_time => {}
                                _ => {
                                    discover.broadcast_req().await;
                                }
                            }
                        }
                        _ = batch_timeout.recv() => {
                            let caches = server.caches.load();
                            batch.retain(|k, _| !caches.contains(k));
                            if !batch.is_empty() {
                                let mut new_caches: CacheData = CacheData::clone(&caches);
                                for (url, ip) in batch.drain() {
                                    tracing::info!(cache = %&*url, %ip, "Found new local binary caches");
                                    local_caches.insert(ip, url.clone());
                                    new_caches.insert_cache(0, local_cache_config.error_strategy, url);
                                }
                                server.caches.store(Arc::new(new_caches));
                            }
                        }
                        Some(bad_cache) = manage_rx.recv() => {
                            let caches = server.caches.load();
                            let mut new_caches = CacheData::clone(&caches);
                            if let Some(meta) = new_caches.get_meta_mut(&bad_cache) {
                                match meta.error_strategy {
                                    ErrorStrategy::Remove => {
                                        new_caches.remove(&bad_cache);
                                        tracing::info!(cache = %&*bad_cache, "Removing bad cache");
                                    }
                                    ErrorStrategy::Timeout { timeout } => {
                                        meta.timed_out_until = Some(Instant::now() + timeout);
                                        tracing::info!(cache = %&*bad_cache, ?timeout, "Timed out cache");
                                    }
                                }
                                server.caches.store(Arc::new(new_caches));
                            }
                        }
                        Some(event) = events.recv() => {
                            match event {
                                Event::NetworkChanged => {
                                    let caches = server.caches.load();
                                    let mut new_caches = CacheData::clone(&caches);
                                    for (_, cache) in local_caches.drain() {
                                        // TODO: if cache priority overwritten by url restore it
                                        new_caches.remove(&cache);
                                    }
                                    server.caches.store(Arc::new(new_caches));
                                    discover.broadcast_req().await;
                                }
                                Event::Goodbye { source_ip } => {
                                    if let Some(url) = local_caches.remove(&source_ip) {
                                        let caches = server.caches.load();
                                        let mut new_caches = CacheData::clone(&caches);
                                        if new_caches.remove(&url) {
                                            tracing::info!(url = %&*url, "Removed local binary cache")
                                        }
                                        server.caches.store(Arc::new(new_caches));
                                    }
                                }
                                Event::Adv {
                                    source_ip,
                                    binary_cache_url,
                                } => {
                                    last_adv = Some(Instant::now());
                                    batch_timer.set_timeout(Duration::from_secs(1)).await;
                                    let url = deduper.get_or_insert(binary_cache_url);
                                    batch.insert(url, source_ip);
                                }
                            }
                        }
                    }
                }
            }
        });

        Ok(ret)
    }
}
