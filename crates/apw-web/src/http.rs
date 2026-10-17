//! 把桌面版的命令原样暴露成 HTTP。这里只做转译，不判断库存。

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use apw_core::apple::AppleClient;
use apw_core::catalog::Catalog;
use apw_core::config::{MIN_INTERVAL_SECONDS, Settings, SettingsStore};
use apw_core::model::{Category, Product, REGIONS, Store, Target, region_by_locale};
use apw_core::notify::Notification;
use apw_core::watcher::{TargetState, Watcher};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Router, http::HeaderMap};
use futures_util::Stream;
use futures_util::stream::unfold;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

const ALERT_MP3: &[u8] = include_bytes!("../../apw-core/assets/alert.mp3");
const NOTICE_LIMIT: usize = 50;

#[derive(Clone)]
pub(crate) struct Outbound {
    pub name: &'static str,
    pub data: String,
    /// 提示的序号。事件流用它丢掉「补发历史」和「实时广播」重叠的那一条。
    pub notice_id: u64,
}

#[derive(Clone)]
struct Notice {
    id: u64,
    text: String,
}

#[derive(Clone)]
pub(crate) struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    watcher: Watcher,
    client: AppleClient,
    catalog: Catalog,
    http: reqwest::Client,
    settings: RwLock<Settings>,
    store: Option<SettingsStore>,
    bus: broadcast::Sender<Outbound>,
    notices: Mutex<Vec<Notice>>,
    next_notice: AtomicU64,
    notifications: mpsc::Sender<Notification>,
}

impl AppState {
    pub(crate) fn new(
        watcher: Watcher,
        client: AppleClient,
        mut settings: Settings,
        store: Option<SettingsStore>,
        notifications: mpsc::Sender<Notification>,
    ) -> Self {
        let catalog = Catalog::new();
        catalog.attach_companions(&mut settings.targets);
        catalog.attach_locations(&mut settings.targets);
        let (bus, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                watcher,
                client,
                catalog,
                http: reqwest::Client::new(),
                settings: RwLock::new(settings),
                store,
                bus,
                notices: Mutex::new(Vec::new()),
                next_notice: AtomicU64::new(1),
                notifications,
            }),
        }
    }

    pub(crate) fn watcher(&self) -> &Watcher {
        &self.inner.watcher
    }

    pub(crate) fn client(&self) -> &AppleClient {
        &self.inner.client
    }

    pub(crate) fn http(&self) -> reqwest::Client {
        self.inner.http.clone()
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Outbound> {
        self.inner.bus.subscribe()
    }

    pub(crate) fn settings_snapshot(&self) -> Settings {
        self.inner
            .settings
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|err| err.into_inner().clone())
    }

    /// 更新内存副本并尝试落盘。落盘失败只报错，不回滚内存。
    pub(crate) fn put_settings(&self, next: Settings) -> Result<(), String> {
        let mut guard = self
            .inner
            .settings
            .write()
            .unwrap_or_else(|err| err.into_inner());
        *guard = next;
        let to_save = guard.clone();
        drop(guard);
        match &self.inner.store {
            Some(store) => store.save(&to_save).map_err(|err| err.to_string()),
            None => Ok(()),
        }
    }

    pub(crate) fn prepare_targets(&self, targets: &mut [Target]) {
        self.inner.catalog.attach_companions(targets);
        self.inner.catalog.attach_locations(targets);
    }

    pub(crate) fn publish_event(&self, event: &apw_core::watcher::Event) {
        self.send(
            "watcher",
            &serde_json::to_string(event).unwrap_or_else(|_| "null".into()),
            0,
        );
    }

    pub(crate) fn publish_open_bag(&self, url: &str) {
        let data = serde_json::to_string(url).unwrap_or_else(|_| "null".into());
        self.send("open-bag", &data, 0);
    }

    pub(crate) fn publish_notice(&self, text: impl Into<String>) {
        let text = text.into();
        let id = self.inner.next_notice.fetch_add(1, Ordering::Relaxed);
        let mut notices = self
            .inner
            .notices
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        notices.push(Notice {
            id,
            text: text.clone(),
        });
        if notices.len() > NOTICE_LIMIT {
            let extra = notices.len() - NOTICE_LIMIT;
            notices.drain(0..extra);
        }
        drop(notices);
        let data = serde_json::to_string(&text).unwrap_or_else(|_| "null".into());
        self.send("notice", &data, id);
    }

    pub(crate) fn notices(&self) -> Vec<(u64, String)> {
        self.inner
            .notices
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .map(|notice| (notice.id, notice.text.clone()))
            .collect()
    }

    pub(crate) async fn enqueue_notification(
        &self,
        notification: Notification,
    ) -> Result<(), String> {
        self.inner
            .notifications
            .send(notification)
            .await
            .map_err(|_| "提醒傳送工作已停止，無法發送到貨提醒".to_string())
    }

    fn send(&self, name: &'static str, data: &str, notice_id: u64) {
        let _ = self.inner.bus.send(Outbound {
            name,
            data: data.to_string(),
            notice_id,
        });
    }
}

pub(crate) fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/api/regions", get(list_regions))
        .route("/api/categories", get(list_categories))
        .route("/api/stores", get(list_stores))
        .route("/api/products", get(list_products))
        .route("/api/products/refresh", post(refresh_products))
        .route("/api/settings", get(get_settings).put(save_settings))
        .route("/api/push-urls", put(set_push_urls))
        .route("/api/snapshot", get(get_snapshot))
        .route("/api/targets", put(set_targets))
        .route("/api/interval", put(set_interval))
        .route("/api/watch/start", post(start_watching))
        .route("/api/watch/stop", post(stop_watching))
        .route("/api/running", get(is_running))
        .route("/api/notify/test", post(test_notify))
        .route("/api/events", get(events))
        .route("/alert.mp3", get(alert_mp3))
        .with_state(state);

    match web_dist() {
        Some(dir) => {
            let index = dir.join("index.html");
            let files = tower_http::services::ServeDir::new(&dir)
                .not_found_service(tower_http::services::ServeFile::new(index));
            api.fallback_service(files)
        }
        None => api,
    }
}

fn web_dist() -> Option<PathBuf> {
    let raw = std::env::var("APW_WEB_DIST").unwrap_or_else(|_| "dist".into());
    let path = PathBuf::from(raw);
    path.join("index.html").is_file().then_some(path)
}

type ApiError = (StatusCode, Json<serde_json::Value>);

fn bad(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegionDto {
    title: &'static str,
    locale: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CategoryDto {
    value: Category,
    title: &'static str,
}

#[derive(Deserialize)]
struct LocaleQuery {
    locale: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshBody {
    locale: String,
    category: Option<Category>,
}

#[derive(Deserialize)]
struct UrlsBody {
    urls: Vec<String>,
}

#[derive(Deserialize)]
struct TargetsBody {
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct IntervalBody {
    seconds: u64,
}

async fn list_regions() -> Json<Vec<RegionDto>> {
    Json(
        REGIONS
            .iter()
            .map(|region| RegionDto {
                title: region.title,
                locale: region.locale,
            })
            .collect(),
    )
}

async fn list_categories() -> Json<Vec<CategoryDto>> {
    Json(
        Category::ALL
            .iter()
            .map(|category| CategoryDto {
                value: *category,
                title: category.title(),
            })
            .collect(),
    )
}

async fn list_stores(
    State(state): State<AppState>,
    Query(query): Query<LocaleQuery>,
) -> Result<Json<Vec<Store>>, ApiError> {
    state
        .inner
        .catalog
        .stores(&query.locale)
        .map(Json)
        .map_err(|err| bad(err.to_string()))
}

async fn list_products(
    State(state): State<AppState>,
    Query(query): Query<LocaleQuery>,
) -> Result<Json<Vec<Product>>, ApiError> {
    state
        .inner
        .catalog
        .products(&query.locale)
        .map(Json)
        .map_err(|err| bad(err.to_string()))
}

async fn refresh_products(
    State(state): State<AppState>,
    Json(body): Json<RefreshBody>,
) -> Result<Json<usize>, ApiError> {
    let region =
        region_by_locale(&body.locale).ok_or_else(|| bad(format!("認不出地區 {}", body.locale)))?;
    let http = state.http();
    state
        .inner
        .catalog
        .refresh_products(region, body.category, &http)
        .await
        .map(Json)
        .map_err(|err| bad(err.to_string()))
}

async fn get_settings(State(state): State<AppState>) -> Json<Settings> {
    Json(state.settings_snapshot())
}

async fn save_settings(
    State(state): State<AppState>,
    Json(settings): Json<Settings>,
) -> Result<Json<Settings>, ApiError> {
    let mut next = settings;
    next.normalize();
    state.prepare_targets(&mut next.targets);
    state.watcher().set_targets(next.targets.clone()).await;
    state.watcher().set_interval(next.interval()).await;
    if next.proxies != state.settings_snapshot().proxies {
        state
            .client()
            .set_proxies(&next.proxies)
            .await
            .map_err(|err| bad(format!("代理設定未生效：{err}")))?;
    }
    state.put_settings(next.clone()).map_err(bad)?;
    Ok(Json(next))
}

async fn set_push_urls(
    State(state): State<AppState>,
    Json(body): Json<UrlsBody>,
) -> Result<Json<Settings>, ApiError> {
    let mut next = state.settings_snapshot();
    next.set_push_urls(&body.urls);
    next.normalize();
    state.put_settings(next.clone()).map_err(bad)?;
    Ok(Json(next))
}

async fn get_snapshot(State(state): State<AppState>) -> Json<Vec<TargetState>> {
    Json(state.watcher().snapshot().await)
}

async fn set_targets(
    State(state): State<AppState>,
    Json(body): Json<TargetsBody>,
) -> Result<Json<Vec<TargetState>>, ApiError> {
    let mut targets = body.targets;
    state.prepare_targets(&mut targets);
    state.watcher().set_targets(targets.clone()).await;
    let mut next = state.settings_snapshot();
    next.targets = targets;
    state.put_settings(next).map_err(bad)?;
    Ok(Json(state.watcher().snapshot().await))
}

async fn set_interval(
    State(state): State<AppState>,
    Json(body): Json<IntervalBody>,
) -> Result<Json<u64>, ApiError> {
    let secs = body.seconds.max(MIN_INTERVAL_SECONDS);
    state
        .watcher()
        .set_interval(Duration::from_secs(secs))
        .await;
    let mut next = state.settings_snapshot();
    next.interval_seconds = secs;
    state.put_settings(next).map_err(bad)?;
    Ok(Json(secs))
}

async fn start_watching(State(state): State<AppState>) -> StatusCode {
    state.watcher().start().await;
    StatusCode::NO_CONTENT
}

async fn stop_watching(State(state): State<AppState>) -> StatusCode {
    state.watcher().stop().await;
    StatusCode::NO_CONTENT
}

async fn is_running(State(state): State<AppState>) -> Json<bool> {
    Json(state.watcher().is_running().await)
}

async fn test_notify(State(state): State<AppState>) -> Result<StatusCode, ApiError> {
    state
        .enqueue_notification(Notification::new(
            "有貨了（測試）",
            "如果你看到並聽到了這則，表示提醒是通的",
        ))
        .await
        .map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn alert_mp3() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mpeg"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    (headers, ALERT_MP3).into_response()
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(event_stream(state)).keep_alive(KeepAlive::default())
}

fn event_stream(state: AppState) -> impl Stream<Item = Result<Event, Infallible>> {
    let rx = state.subscribe();
    let mut backlog = state.notices();
    backlog.reverse();
    unfold(
        (rx, backlog, 0u64),
        |(mut rx, mut backlog, mut seen)| async move {
            if let Some((id, text)) = backlog.pop() {
                seen = seen.max(id);
                let data = serde_json::to_string(&text).unwrap_or_else(|_| "null".into());
                let event = Event::default().event("notice").data(data);
                return Some((Ok(event), (rx, backlog, seen)));
            }
            loop {
                match rx.recv().await {
                    Ok(message) => {
                        if message.name == "notice" && message.notice_id <= seen {
                            continue;
                        }
                        if message.name == "notice" {
                            seen = message.notice_id;
                        }
                        let event = Event::default().event(message.name).data(message.data);
                        return Some((Ok(event), (rx, backlog, seen)));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use apw_core::apple::{AppleClient, ClientConfig};
    use apw_core::config::Settings;
    use apw_core::watcher::Watcher;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    async fn test_state() -> AppState {
        let client = AppleClient::new(ClientConfig::default()).expect("客户端");
        let (watcher, _events, engine) =
            Watcher::new(client.clone(), apw_core::watcher::WatcherConfig::default());
        tokio::spawn(engine);
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        AppState::new(watcher, client, Settings::default(), None, tx)
    }

    #[tokio::test]
    async fn regions_come_from_the_offline_catalog() {
        let app = router(test_state().await);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/regions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let regions: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert!(regions.iter().any(|region| region["locale"] == "zh_CN"));
    }

    #[tokio::test]
    async fn alert_sound_is_the_embedded_file() {
        let app = router(test_state().await);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/alert.mp3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes.as_ref(), ALERT_MP3);
    }
}
