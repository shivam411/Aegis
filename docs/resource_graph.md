# Aegis Resource Graph Architecture Specification

This specification defines the unified Aegis Resource Graph traversal model.

Rather than managing decoupled resources (`Project`, `Deployment`, `Release`, `Process`) independently, Aegis models every project as a connected DAG (Directed Acyclic Graph) rooted at `Project`.

---

## 1. Resource Graph Hierarchy

```
Project (Root Aggregate)
   │
   ├── Releases (Version History & Checksums)
   │     └── Stored Artifacts (Binaries, Zips, SHA256 Manifests)
   │
   ├── Deployments (Strategy Executions)
   │     └── Build Pipeline Stages (7-Stage Execution Events)
   │
   ├── Runtime Engine (Node.js, Rust, Go, Python, Generic)
   │     └── Monitored Processes (PID, Health Probes, Restart Count)
   │
   ├── Scheduler (Daily Auto-Deployment Cron Rules)
   │
   ├── Metrics & Observability (CPU, Memory, Request Latency)
   │
   ├── Process Logs (Tail Streams & System Logs)
   │
   └── Domain Event Stream (Immutable Audit Log Replay)
```

---

## 2. Resource Graph Traversal API (`aegis inspect`)

Command invocation `aegis inspect <PROJECT_ID>` traverses the graph:

```json
{
  "project_id": "01910a3b-7f12-7890-8b01-123456789abc",
  "name": "my-api",
  "status": "Active",
  "runtime": {
    "engine": "Rust",
    "processes": [
      {
        "process_id": "01910a3b-7f12-7890-8b01-222222222222",
        "pid": 1001,
        "status": "Running",
        "health": "Healthy"
      }
    ]
  },
  "active_release": {
    "release_id": "01910a3b-7f12-7890-8b01-333333333333",
    "version": "v1.0.0",
    "commit_sha": "abc1234"
  },
  "deployments_summary": {
    "total": 5,
    "successful": 5,
    "last_strategy": "GracefulSwitch"
  },
  "schedules": [
    {
      "target_hour": 2,
      "target_minute": 0,
      "enabled": true
    }
  ]
}
```

---

## 3. Advantages of the Resource Graph
1. **Unified Navigation**: TUI dashboard can drill down from project overview directly into active process, release artifacts, or build pipeline logs.
2. **AI Copilot Context**: AI agents can query a single comprehensive snapshot for root cause diagnosis.
3. **Graph Consistency**: Event sourcing guarantees that project state mutations cascade predictably through the graph.
