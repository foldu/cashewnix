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

#[derive(serde::Deserialize, Clone, Debug, Copy)]
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
    priority_map: BTreeMap<u8, BTreeSet<Url>>,
    caches: BTreeMap<Url, CacheMeta>,
}

#[derive(Clone, Copy)]
struct CacheMeta {
    priority: u8,
    error_strategy: ErrorStrategy,
    timed_out_until: Option<Instant>,
}

impl CacheData {
    fn iter_priorities(&self) -> impl Iterator<Item = (&u8, &BTreeSet<Url>)> {
        self.priority_map.iter()
    }

    fn contains(&self, cache: &Url) -> bool {
        self.caches.contains_key(cache)
    }

    fn insert_cache(&mut self, priority: u8, error_strategy: ErrorStrategy, cache: Url) {
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

    fn remove(&mut self, url: &Url) -> bool {
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

    fn get_meta(&self, url: &Url) -> Option<&CacheMeta> {
        self.caches.get(url)
    }

    fn get_meta_mut(&mut self, url: &Url) -> Option<&mut CacheMeta> {
        self.caches.get_mut(url)
    }
}

#[axum::debug_handler]
#[tracing::instrument(skip(server))]
async fn proxy(server: State<Arc<Server>>, req: Request) -> impl IntoResponse {
    let caches = server.caches.load();
    let path = req.uri().path();
    let query = req.uri().query();

    for (priority, urls) in caches.iter_priorities() {
        tracing::debug!(priority = priority, caches = ?urls, "Trying");
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
                    let mut url = cache.clone();
                    url.set_path(path);
                    url.set_query(query);
                    url
                };
                let client = server.client.clone();
                let headers = req.headers().clone();
                async move {
                    match client.get(url).headers(headers).send().await {
                        Ok(resp) => Ok(resp),
                        Err(e) => Err((e, cache)),
                    }
                }
            });
        }

        let timeout = tokio::time::sleep(
            server
                .config
                .priority_config
                .get(priority)
                .map(|entry| entry.timeout)
                .unwrap_or(Duration::from_secs(5)),
        );
        tokio::pin!(timeout);

        // TODO: error on missed timeout

        let start = Instant::now();
        let first_hit = loop {
            tokio::select! {
                _ = &mut timeout => {
                    break None;
                }
                Some(resp) = requests.next() => {
                    let found = match resp {
                        Ok(resp) if resp.status() == StatusCode::OK => Some(resp),
                        Ok(resp) => {
                            tracing::debug!(url = %resp.url(), status = %resp.status(), "Got bad status");
                            None
                        },
                        Err((error, cache)) => {
                            tracing::error!(%cache, %error, "Error fetching from cache");
                            server.manage_tx.send(cache).await.unwrap();
                            None
                        }
                    };
                    if let Some(url) = found {
                        break Some(url);
                    }
                }
                else => break None,
            }
        };

        if let Some(resp) = first_hit {
            tracing::debug!(
                hit = %resp.url(),
                "Took {:?} to find first hit",
                start.elapsed()
            );
            return Response::from(resp);
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
    manage_tx: mpsc::Sender<Url>,
}

impl Server {
    pub async fn new(
        set: &mut tokio::task::JoinSet<Result<(), eyre::Error>>,
        config: Config,
        keystore: KeyStore,
        token: CancellationToken,
    ) -> Result<Arc<Self>, eyre::Error> {
        let mut caches = CacheData::default();
        for host in &config.binary_caches {
            caches.insert_cache(host.priority, host.error_strategy, host.url.clone());
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

                discover.broadcast_req().await;

                let mut interval = tokio::time::interval(local_cache_config.discovery_refresh_time);
                let (batch_timer, mut batch_timeout) = DynamicTimer::new();
                let mut batch: HashMap<Url, std::net::IpAddr> = HashMap::default();
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
                                    tracing::info!(cache = %url, %ip, "Found new local binary caches");
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
                                        tracing::info!(cache = %bad_cache, "Removing bad cache");
                                    }
                                    ErrorStrategy::Timeout { timeout } => {
                                        meta.timed_out_until = Some(Instant::now() + timeout);
                                        tracing::info!(cache = %bad_cache, ?timeout, "Timed out cache");
                                    }
                                }
                                server.caches.store(Arc::new(new_caches));
                            }
                        }
                        Some(event) = events.recv() => {
                            match event {
                                  Event::Goodbye { source_ip } => {
                                      if let Some(url) = local_caches.remove(&source_ip) {
                                          let caches = server.caches.load();
                                          let mut new_caches = CacheData::clone(&caches);
                                          if new_caches.remove(&url) {
                                              tracing::info!(%url, "Removed local binary cache")
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
                                      batch.insert(binary_cache_url, source_ip);
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
