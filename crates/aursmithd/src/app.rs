//! 共享服务状态。

use crate::{aur::AurClient, config::Config, review::Reviewer};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

pub type BuilderSeen = Arc<Mutex<Option<(String, DateTime<Utc>)>>>;

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    pub aur: AurClient,
    pub reviewer: Option<Arc<Reviewer>>,
    /// 唤醒后台调和循环（订阅、审批、构建回报之后立即推进，而不是等下一个周期）。
    pub wake: Arc<Notify>,
    /// 最近一次 Builder 租约请求（builder_id, 时间），用于状态页。
    pub builder_seen: BuilderSeen,
}

impl AppState {
    pub fn new(db: SqlitePool, config: Config, aur: AurClient, reviewer: Option<Reviewer>) -> Self {
        Self {
            db,
            config: Arc::new(config),
            aur,
            reviewer: reviewer.map(Arc::new),
            wake: Arc::new(Notify::new()),
            builder_seen: Arc::new(Mutex::new(None)),
        }
    }

    pub fn mark_builder_seen(&self, builder_id: &str) {
        if let Ok(mut seen) = self.builder_seen.lock() {
            *seen = Some((builder_id.to_owned(), Utc::now()));
        }
    }

    pub fn builder_last_seen(&self) -> Option<(String, DateTime<Utc>)> {
        self.builder_seen.lock().ok().and_then(|seen| seen.clone())
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }
}
