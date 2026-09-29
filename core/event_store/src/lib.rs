use aegis_types::{Event, EventId};
use sqlx::SqlitePool;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, Mutex};

/// A callback invoked synchronously, in sequence order, for every appended event.
pub type SyncListener = Arc<dyn Fn(&Event) + Send + Sync>;

#[derive(Clone)]
pub struct EventStore {
    pool: SqlitePool,
    tx: broadcast::Sender<Event>,
    /// Serializes insert + dispatch so listeners and subscribers observe events
    /// in exactly the order they were persisted.
    append_lock: Arc<Mutex<()>>,
    sync_listeners: Arc<RwLock<Vec<SyncListener>>>,
}

impl EventStore {
    pub fn new(pool: SqlitePool) -> Self {
        let (tx, _) = broadcast::channel(4096);
        Self {
            pool,
            tx,
            append_lock: Arc::new(Mutex::new(())),
            sync_listeners: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// The database, for other tables kept alongside the events (metrics).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Performs SQLite database migrations.
    pub async fn initialize_db(pool: &SqlitePool) -> Result<(), anyhow::Error> {
        tracing::info!("Running database migrations...");
        sqlx::migrate!("./migrations").run(pool).await?;
        tracing::info!("Database migrations complete.");
        Ok(())
    }

    /// Registers a listener that runs for every appended event before it is
    /// broadcast. Used by read-model projections that must never miss or
    /// reorder events.
    pub fn add_sync_listener(&self, listener: SyncListener) {
        self.sync_listeners.write().unwrap().push(listener);
    }

    /// Appends a new event, saves it to the SQLite database, and publishes it to active subscribers.
    pub async fn append_event(
        &self,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<Event, anyhow::Error> {
        let payload_json = serde_json::to_string(&payload)?;
        let event = Event::new(event_type, payload_json);

        let _guard = self.append_lock.lock().await;
        sqlx::query(
            "INSERT INTO events (id, event_type, payload_json, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(event.id.to_string())
        .bind(&event.event_type)
        .bind(&event.payload_json)
        .bind(&event.created_at)
        .execute(&self.pool)
        .await?;

        let listeners = self.sync_listeners.read().unwrap().clone();
        for listener in listeners {
            listener(&event);
        }
        let _ = self.tx.send(event.clone());

        tracing::debug!(id = %event.id, event_type = %event.event_type, "Event persisted and dispatched");

        Ok(event)
    }

    /// Gets the total count of events directly via SQL COUNT query.
    pub async fn get_event_count(&self) -> Result<u64, anyhow::Error> {
        use sqlx::Row;
        let row = sqlx::query("SELECT COUNT(*) FROM events")
            .fetch_one(&self.pool)
            .await?;
        let count: i64 = row.try_get(0)?;
        let count = u64::try_from(count)
            .map_err(|_| anyhow::anyhow!("Event count from DB was negative: {}", count))?;
        Ok(count)
    }

    /// Fetches all stored events in insertion order.
    pub async fn get_events(&self) -> Result<Vec<Event>, anyhow::Error> {
        let rows = sqlx::query(
            "SELECT id, event_type, payload_json, created_at FROM events ORDER BY seq ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            use sqlx::Row;
            let id_str: String = row.try_get(0)?;
            let id: EventId = id_str
                .parse()
                .map_err(|e| anyhow::anyhow!("Failed to parse EventId: {}", e))?;
            let mut ev = Event::new(row.try_get::<String, _>(1)?, row.try_get::<String, _>(2)?);
            ev.id = id;
            ev.created_at = row.try_get(3)?;
            events.push(ev);
        }

        Ok(events)
    }

    /// Obtains a subscription receiver to listen to incoming events in real time.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn memory_store() -> EventStore {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        EventStore::initialize_db(&pool).await.unwrap();
        EventStore::new(pool)
    }

    #[tokio::test]
    async fn test_event_store_flow() {
        let store = memory_store().await;
        let mut rx = store.subscribe();

        let payload = serde_json::json!({ "repo_url": "https://github.com/shivam411/Aegis" });
        let event = store
            .append_event("RepositoryAdded", payload.clone())
            .await
            .unwrap();

        assert_eq!(event.event_type, "RepositoryAdded");
        assert!(event.payload_json.contains("shivam411/Aegis"));

        let received = rx.recv().await.unwrap();
        assert_eq!(received.id, event.id);
        assert_eq!(received.event_type, "RepositoryAdded");

        let events = store.get_events().await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, event.id);
    }

    #[tokio::test]
    async fn test_replay_follows_insertion_order_and_listeners_run() {
        let store = memory_store().await;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        store.add_sync_listener(Arc::new(move |e: &Event| {
            seen_clone.lock().unwrap().push(e.event_type.clone());
        }));

        for i in 0..20 {
            store
                .append_event(&format!("E{i}"), serde_json::json!({}))
                .await
                .unwrap();
        }

        let replayed: Vec<String> = store
            .get_events()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.event_type)
            .collect();
        let expected: Vec<String> = (0..20).map(|i| format!("E{i}")).collect();
        assert_eq!(replayed, expected);
        assert_eq!(*seen.lock().unwrap(), expected);
    }

    #[tokio::test]
    async fn test_seq_migration_preserves_existing_events() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        // Simulate a database created before the seq migration.
        sqlx::query(include_str!("../migrations/20260716000000_init.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO events VALUES ('01a0e66a-2697-73c0-bff5-3346fccc6ad3', 'Old', '{}', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let migrator = sqlx::migrate!("./migrations");
        let init = migrator.iter().next().unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (?, ?, 1, ?, 0)")
            .bind(init.version)
            .bind(init.description.as_ref())
            .bind(init.checksum.as_ref())
            .execute(&pool)
            .await
            .unwrap();

        EventStore::initialize_db(&pool).await.unwrap();
        let store = EventStore::new(pool);
        store
            .append_event("New", serde_json::json!({}))
            .await
            .unwrap();

        let types: Vec<String> = store
            .get_events()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.event_type)
            .collect();
        assert_eq!(types, vec!["Old".to_string(), "New".to_string()]);
    }
}
