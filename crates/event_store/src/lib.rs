use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub event_type: String,
    pub payload_json: String,
    pub created_at: String,
}

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
        sqlx::migrate!("./migrations")
            .run(pool)
            .await?;
        tracing::info!("Database migrations complete.");
        Ok(())
    }

    /// Appends a new event, saves it to the SQLite database, and publishes it to active subscribers.
    pub async fn append_event(
        &self,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<Event, anyhow::Error> {
        let id = Uuid::new_v4().to_string();
        let payload_json = serde_json::to_string(&payload)?;
        let created_at = Utc::now().to_rfc3339();

        let event = Event {
            id: id.clone(),
            event_type: event_type.to_string(),
            payload_json,
            created_at,
        };

        sqlx::query(
            "INSERT INTO events (id, event_type, payload_json, created_at) VALUES (?, ?, ?, ?)"
        )
        .bind(&event.id)
        .bind(&event.event_type)
        .bind(&event.payload_json)
        .bind(&event.created_at)
        .execute(&self.pool)
        .await?;

        let _ = self.tx.send(event.clone());

        tracing::info!(id = %event.id, event_type = %event.event_type, "Event persisted and dispatched");

        Ok(event)
    }

    /// Fetches all stored events.
    pub async fn get_events(&self) -> Result<Vec<Event>, anyhow::Error> {
        let rows = sqlx::query("SELECT id, event_type, payload_json, created_at FROM events ORDER BY created_at ASC")
            .fetch_all(&self.pool)
            .await?;

        let mut events = Vec::new();
        for row in rows {
            use sqlx::Row;
            events.push(Event {
                id: row.try_get(0)?,
                event_type: row.try_get(1)?,
                payload_json: row.try_get(2)?,
                created_at: row.try_get(3)?,
            });
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
        let event = store.append_event("RepositoryAdded", payload.clone()).await.unwrap();

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

