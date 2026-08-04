use aegis_event_bus::EventBus;
use aegis_projection::ProjectionEngine;
use aegis_types::{Event, ProjectId};
use criterion::{criterion_group, criterion_main, Criterion};

fn benchmark_event_bus_publish(c: &mut Criterion) {
    let bus = EventBus::new();
    let proj_id = ProjectId::new();

    c.bench_function("event_bus_publish_single", |b| {
        b.iter(|| {
            let event = Event::new(
                "DeploymentQueued",
                serde_json::json!({
                    "project_id": proj_id.to_string(),
                    "branch": "main"
                })
                .to_string(),
            );
            bus.publish(event);
        });
    });
}

fn benchmark_projection_folding(c: &mut Criterion) {
    let engine = ProjectionEngine::new();
    let proj_id = ProjectId::new();
    let event = Event::new(
        "ProjectCreated",
        serde_json::json!({
            "project_id": proj_id.to_string(),
            "name": "benchmark-app",
            "repository_url": "https://github.com/aegis/benchmark-app",
            "branch": "main"
        })
        .to_string(),
    );

    c.bench_function("projection_engine_apply_event", |b| {
        b.iter(|| {
            engine.apply_event(&event);
        });
    });
}

criterion_group!(
    benches,
    benchmark_event_bus_publish,
    benchmark_projection_folding
);
criterion_main!(benches);
