CREATE TABLE alert_notifications (
    id TEXT PRIMARY KEY NOT NULL,
    alert_id TEXT NOT NULL REFERENCES alerts(id) ON DELETE CASCADE,
    alert_state TEXT NOT NULL,
    channel TEXT NOT NULL CHECK(channel IN ('webhook', 'ntfy')),
    status TEXT NOT NULL CHECK(status IN ('pending', 'delivered', 'failed')),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    delivered_at TEXT,
    UNIQUE(alert_id, alert_state, channel)
);
