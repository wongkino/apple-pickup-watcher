//! 浏览器版外壳。
//!
//! 库存判定、合并查询和到货去重仍在 `apw-core`。这一层只提供网页和与桌面版
//! 同一组命令：Apprise 在这里发，提示音和购物袋由浏览器自己播放、打开。

mod http;
mod queue;

use apw_core::apple::{AppleClient, ClientConfig};
use apw_core::bag::AutoOpenBag;
use apw_core::config::load_launch_settings;
use apw_core::model::region_by_locale;
use apw_core::notify::{Apprise, Multi, Notification, Notifier};
use apw_core::watcher::{Event, Watcher, WatcherConfig};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::http::{AppState, router};

#[tokio::main]
async fn main() {
    let launch = load_launch_settings();
    let client = match AppleClient::new(ClientConfig {
        proxies: launch.settings.proxies.clone(),
        ..ClientConfig::default()
    }) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("构造 Apple 客户端失败：{err}");
            std::process::exit(1);
        }
    };

    let (watcher, events, engine) = Watcher::new(client.clone(), WatcherConfig::default());
    tokio::spawn(engine);
    watcher.set_targets(launch.settings.targets.clone()).await;
    watcher.set_interval(launch.settings.interval()).await;

    let (notifications, pending) = queue::channel();
    let state = AppState::new(
        watcher,
        client,
        launch.settings,
        launch.store,
        notifications,
    );
    for notice in launch.notices {
        state.publish_notice(notice);
    }

    let notify_state = state.clone();
    tokio::spawn(queue::run(pending, move |notification| {
        let state = notify_state.clone();
        async move {
            if let Err(err) = dispatch_notification(&state, notification).await {
                state.publish_notice(format!("發送提醒時出錯：{err}"));
            }
        }
    }));
    tokio::spawn(pump_events(state.clone(), events));

    let addr = std::env::var("APW_WEB_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("无法监听 {addr}：{err}");
            std::process::exit(1);
        }
    };
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or(addr);
    eprintln!("Apple Pickup Watcher web UI: http://{local}");

    let server = axum::serve(listener, router(state)).with_graceful_shutdown(shutdown());
    if let Err(err) = server.await {
        eprintln!("网页服务退出：{err}");
        std::process::exit(1);
    }
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => {
                    ctrl_c.await;
                    return;
                }
            };
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}

async fn dispatch_notification(
    state: &AppState,
    notification: Notification,
) -> Result<(), apw_core::notify::NotifyError> {
    let settings = state.settings_snapshot();
    let mut channels = Multi::new();
    for apprise in Apprise::from_list(&settings.push_urls()) {
        channels.push(apprise);
    }
    if channels.is_empty() {
        return Ok(());
    }
    channels.notify(&notification).await
}

async fn pump_events(state: AppState, mut events: mpsc::Receiver<Event>) {
    let mut auto_open_bag = AutoOpenBag::default();
    while let Some(event) = events.recv().await {
        state.publish_event(&event);

        if let Event::RunStateChanged { running } = &event {
            auto_open_bag.on_run_state_changed(*running);
        }

        if let Event::InStock {
            state: target_state,
        } = &event
        {
            let target = &target_state.target;
            let notification = Notification::new(
                "有貨了",
                format!("{} {}", target.store_title, target.product_name),
            );
            let notification = match region_by_locale(&target.locale) {
                Some(region) => notification.with_url(region.bag_url()),
                None => notification,
            };
            let settings = state.settings_snapshot();
            let _ = auto_open_bag.open_if_needed(settings.open_bag_on_hit, &target.locale, |url| {
                state.publish_open_bag(url);
                Ok::<(), &str>(())
            });
            if let Err(err) = state.enqueue_notification(notification).await {
                state.publish_notice(err);
            }
        }
    }
}
