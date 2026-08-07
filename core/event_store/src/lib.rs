use aegis_types::{Event, EventId};
use sqlx::SqlitePool;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct EventStore {
    pool: SqlitePool,
    tx: broadcast::Sender<Event>,
}

impl EventStore {
    pub fn new(pool: SqlitePool) -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { pool, tx }
    }

    /// Performs SQLite database migrations.
    pub async fn initialize_db(pool: &SqlitePool) -> Result<(), anyhow::Error> {
        tracing::info!("Running database migrations...");
        sqlx::migrate!("./migrations").run(pool).await?;
        tracing::info!("Database migrations complete.");
        Ok(())
    }

    /// Appends a new event, saves it to the SQLite database, and publishes it to active subscribers.
    pub async fn append_event(
        &self,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<Event, anyhow::Error> {
        let _id = EventId::new();
        let payload_json = serde_json::to_string(&payload)?;
        let event = Event::new(event_type, payload_json);

        let id_str = event.id.to_string();
        sqlx::query(
            "INSERT INTO events (id, event_type, payload_json, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&id_str)
        .bind(&event.event_type)
        .bind(&event.payload_json)
        .bind(&event.created_at)
        .execute(&self.pool)
        .await?;

        let _ = self.tx.send(event.clone());

        tracing::info!(id = %event.id, event_type = %event.event_type, "Event persisted and dispatched");

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

    /// Fetches all stored events.
    pub async fn get_events(&self) -> Result<Vec<Event>, anyhow::Error> {
        let rows = sqlx::query(
            "SELECT id, event_type, payload_json, created_at FROM events ORDER BY created_at ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::new();
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

    #[tokio::test]
    async fn test_event_store_flow() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        EventStore::initialize_db(&pool).await.unwrap();

        let store = EventStore::new(pool);
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
}
