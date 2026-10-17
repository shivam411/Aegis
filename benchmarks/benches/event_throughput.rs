//! Event pipeline benchmarks: publishing on the in-memory bus, appending to
//! the SQLite event store (file-backed, as the daemon opens it), reading the
//! whole history back, and rebuilding the projection from it (what the
//! daemon does on every start).

use aegis_benchmarks::{deploy_history, history_events, open_store, seed_events};
use aegis_event_bus::EventBus;
use aegis_projection::ProjectionEngine;
use aegis_types::{Event, ProjectId};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use std::time::Duration;

/// 20 apps × 50 deployments × 12 events + 20 = 12,020 events.
const PROJECTS: usize = 20;
const DEPLOYS: usize = 50;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn event_bus_publish(c: &mut Criterion) {
    let bus = EventBus::new();
    // A subscriber, so publishing actually clones into a channel.
    let _rx = bus.subscribe();
    let proj_id = ProjectId::new();

    c.bench_function("event_bus/publish", |b| {
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

fn projection_apply(c: &mut Criterion) {
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

    c.bench_function("projection/apply_event", |b| {
        b.iter(|| engine.apply_event(&event));
    });
}

fn event_store_append(c: &mut Criterion) {
    let rt = runtime();
    let dir = tempfile::TempDir::new().unwrap();
    let store = rt.block_on(open_store(&dir.path().join("aegis.db")));
    // Something listening, like the daemon's projection and SSE streams.
    let projection = ProjectionEngine::new();
    store.add_sync_listener(std::sync::Arc::new(move |e: &Event| {
        projection.apply_event(e)
    }));
    let history = deploy_history(1, 200);

    let mut group = c.benchmark_group("event_store");
    group.throughput(Throughput::Elements(1));
    group.bench_function("append", |b| {
        let mut i = 0usize;
        b.to_async(&rt).iter(|| {
            let (kind, payload) = history[i % history.len()].clone();
            i += 1;
            let store = store.clone();
            async move { store.append_event(&kind, payload).await.unwrap() }
        });
    });
    group.finish();
}

fn event_store_replay_and_rebuild(c: &mut Criterion) {
    let rt = runtime();
    let dir = tempfile::TempDir::new().unwrap();
    let store = rt.block_on(open_store(&dir.path().join("aegis.db")));
    let history = deploy_history(PROJECTS, DEPLOYS);
    rt.block_on(seed_events(&store, &history));
    let n = history.len() as u64;

    let mut group = c.benchmark_group("event_store");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));
    group.throughput(Throughput::Elements(n));
    group.bench_function(format!("replay_{n}"), |b| {
        b.to_async(&rt).iter(|| {
            let store = store.clone();
            async move {
                let events = store.get_events().await.unwrap();
                assert_eq!(events.len() as u64, n);
                events
            }
        });
    });
    group.finish();

    let events = history_events(&history);
    let mut group = c.benchmark_group("projection");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));
    group.throughput(Throughput::Elements(n));
    group.bench_function(format!("rebuild_{n}"), |b| {
        b.iter_batched(
            ProjectionEngine::new,
            |engine| {
                engine.replay_from_store(&events);
                engine
            },
            BatchSize::LargeInput,
        );
    });
    // Startup as the daemon does it: read everything, then fold it.
    group.bench_function(format!("load_and_rebuild_{n}"), |b| {
        b.to_async(&rt).iter(|| {
            let store = store.clone();
            async move {
                let engine = ProjectionEngine::new();
                engine.replay_from_store(&store.get_events().await.unwrap());
                assert_eq!(engine.get_projects().len(), PROJECTS);
                engine
            }
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    event_bus_publish,
    projection_apply,
    event_store_append,
    event_store_replay_and_rebuild
);
criterion_main!(benches);
