use aegis_types::Event;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx }
    }

    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_bus_pub_sub() {
        let bus = EventBus::default();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        let event = Event::new("DeploymentStarted", r#"{"deployment_id":"dep-1"}"#);
        bus.publish(event.clone());

        let recv1 = rx1.recv().await.unwrap();
        let recv2 = rx2.recv().await.unwrap();

        assert_eq!(recv1.id, event.id);
        assert_eq!(recv1.event_type, "DeploymentStarted");
        assert_eq!(recv2.id, event.id);
    }
}
