pub trait Projection: Send + Sync {
    type Event;
    fn apply(&mut self, event: &Self::Event);
}

pub struct ProjectionEngine {}

impl ProjectionEngine {
    pub fn new() -> Self {
        Self {}
    }
}
