//! 有界通知队列。慢速推送不应让一轮到货事件卡住界面与启停状态。

use std::future::Future;

use tokio::sync::mpsc;
use tokio::task::JoinSet;

const QUEUE_CAPACITY: usize = 256;
const CONCURRENCY: usize = 4;

pub(crate) fn channel<T>() -> (mpsc::Sender<T>, mpsc::Receiver<T>) {
    mpsc::channel(QUEUE_CAPACITY)
}

pub(crate) async fn run<T, F, Fut>(mut incoming: mpsc::Receiver<T>, mut dispatch: F)
where
    T: Send + 'static,
    F: FnMut(T) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut active = JoinSet::new();
    loop {
        if active.len() == CONCURRENCY {
            let _ = active.join_next().await;
            continue;
        }
        tokio::select! {
            item = incoming.recv() => match item {
                Some(item) => { active.spawn(dispatch(item)); }
                None => break,
            },
            _ = active.join_next(), if !active.is_empty() => {},
        }
    }
    while active.join_next().await.is_some() {}
}
