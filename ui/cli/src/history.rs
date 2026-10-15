//! Event history as the CLI sees it: parsed timestamps and payloads, time
//! windows, and replaying history into a local projection.

use aegis_api::aegis::EventResponse;
use aegis_projection::ProjectionEngine;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Utc};
use serde_json::Value;

/// One recorded event with its payload parsed.
#[derive(Debug, Clone)]
pub struct Ev {
    pub event_type: String,
    pub payload: Value,
    pub payload_json: String,
    pub at: DateTime<Utc>,
}

impl Ev {
    /// Events whose timestamp doesn't parse are dropped: they can't be placed
    /// on a timeline.
    pub fn from_response(e: EventResponse) -> Option<Self> {
        let at = DateTime::parse_from_rfc3339(&e.created_at)
            .ok()?
            .with_timezone(&Utc);
        Some(Self {
            payload: serde_json::from_str(&e.payload_json).unwrap_or_default(),
            event_type: e.event_type,
            payload_json: e.payload_json,
            at,
        })
    }

    pub fn str(&self, key: &str) -> &str {
        self.payload.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }

    pub fn i64(&self, key: &str) -> Option<i64> {
        self.payload.get(key).and_then(|v| v.as_i64())
    }

    pub fn bool(&self, key: &str) -> bool {
        self.payload
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Who caused the event (`cli`, `user:alice`, ...), empty for the daemon itself.
    pub fn actor(&self) -> &str {
        self.str("actor")
    }

    pub fn deployment_id(&self) -> &str {
        self.str("deployment_id")
    }

    pub fn log_tail(&self) -> Vec<String> {
        self.payload
            .get("log_tail")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn to_event(&self) -> aegis_types::Event {
        let mut event = aegis_types::Event::new(self.event_type.clone(), self.payload_json.clone());
        event.created_at = self.at.to_rfc3339();
        event
    }
}

/// Converts a daemon response, keeping store order.
pub fn from_responses(events: Vec<EventResponse>) -> Vec<Ev> {
    events.into_iter().filter_map(Ev::from_response).collect()
}

/// Parses `--at`: RFC 3339 (`2026-08-03T09:15:00Z`), or a date/time without
/// an offset, which is taken as UTC (`2026-08-03 09:15`, `2026-08-03`).
pub fn parse_at(s: &str) -> anyhow::Result<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, format) {
            return Ok(t.and_utc());
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc());
    }
    anyhow::bail!(
        "Invalid time '{}'. Use RFC 3339, e.g. 2026-08-03T09:15:00Z (times without an offset are UTC).",
        s
    )
}

/// Parses a window length such as `90s`, `30m`, `2h`, `1d` or `1h30m`.
pub fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    let invalid = || anyhow::anyhow!("Invalid duration '{}'. Use e.g. 30m, 2h, 1d or 1h30m.", s);
    let s = s.trim();
    if s.is_empty() {
        return Err(invalid());
    }
    let mut total = Duration::zero();
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: i64 = digits.parse().map_err(|_| invalid())?;
        digits.clear();
        total += match c {
            's' => Duration::seconds(n),
            'm' => Duration::minutes(n),
            'h' => Duration::hours(n),
            'd' => Duration::days(n),
            'w' => Duration::weeks(n),
            _ => return Err(invalid()),
        };
    }
    if !digits.is_empty() || total <= Duration::zero() {
        return Err(invalid());
    }
    Ok(total)
}

/// "2h 5m", "40s", "1.2s": a short human form of a time span.
pub fn human(d: Duration) -> String {
    let ms = d.num_milliseconds().abs();
    if ms < 10_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let secs = ms / 1000;
    match secs {
        0..=119 => format!("{}s", secs),
        120..=7199 => format!("{}m {}s", secs / 60, secs % 60),
        7200..=172_799 => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

/// Folds the events recorded up to and including `until` (all of them when
/// `None`) into a fresh projection. Returns it and how many events were used.
pub fn replay_until(events: &[Ev], until: Option<DateTime<Utc>>) -> (ProjectionEngine, usize) {
    let engine = ProjectionEngine::new();
    let mut used = 0;
    for e in events {
        // Store order is authoritative; timestamps only decide the cut-off.
        if until.is_some_and(|t| e.at > t) {
            break;
        }
        engine.apply_event(&e.to_event());
        used += 1;
    }
    (engine, used)
}

/// The events at or after `since`.
pub fn window(events: &[Ev], since: DateTime<Utc>) -> &[Ev] {
    let start = events.partition_point(|e| e.at < since);
    &events[start..]
}

/// Builds an event for tests.
#[cfg(test)]
pub fn ev(event_type: &str, at: &str, payload: Value) -> Ev {
    Ev::from_response(EventResponse {
        id: format!("{}-{}", event_type, at),
        event_type: event_type.to_string(),
        payload_json: payload.to_string(),
        created_at: at.to_string(),
    })
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_times() {
        let t = parse_at("2026-08-03T09:15:00Z").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-08-03T09:15:00+00:00");
        assert_eq!(parse_at("2026-08-03T11:15:00+02:00").unwrap(), t);
        assert_eq!(parse_at("2026-08-03 09:15").unwrap(), t);
        assert_eq!(parse_at("2026-08-03T09:15:00").unwrap(), t);
        assert_eq!(
            parse_at("2026-08-03").unwrap().to_rfc3339(),
            "2026-08-03T00:00:00+00:00"
        );
        let err = parse_at("yesterday").unwrap_err().to_string();
        assert!(err.contains("RFC 3339"), "{}", err);
        assert!(parse_at("2026-13-40T00:00:00Z").is_err());
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("2h").unwrap(), Duration::hours(2));
        assert_eq!(parse_duration("90s").unwrap(), Duration::seconds(90));
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::minutes(90));
        assert_eq!(parse_duration("1d").unwrap(), Duration::days(1));
        for bad in ["", "2", "h", "2x", "0m", "-1h", "2h5"] {
            assert!(parse_duration(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn human_spans() {
        assert_eq!(human(Duration::milliseconds(1234)), "1.2s");
        assert_eq!(human(Duration::seconds(40)), "40s");
        assert_eq!(human(Duration::seconds(125)), "2m 5s");
        assert_eq!(human(Duration::minutes(150)), "2h 30m");
        assert_eq!(human(Duration::hours(50)), "2d 2h");
    }

    #[test]
    fn replays_up_to_a_time() {
        let p = "01900000-0000-7000-8000-000000000001";
        let r1 = "01900000-0000-7000-8000-0000000000a1";
        let r2 = "01900000-0000-7000-8000-0000000000a2";
        let events = vec![
            ev(
                "ProjectCreated",
                "2026-08-03T09:00:00Z",
                json!({"project_id": p, "name": "app"}),
            ),
            ev(
                "ReleaseCreated",
                "2026-08-03T09:01:00Z",
                json!({"project_id": p, "release_id": r1, "version": "r1"}),
            ),
            ev(
                "ReleasePromoted",
                "2026-08-03T09:02:00Z",
                json!({"project_id": p, "release_id": r1}),
            ),
            ev(
                "ReleaseCreated",
                "2026-08-03T10:01:00Z",
                json!({"project_id": p, "release_id": r2, "version": "r2"}),
            ),
            ev(
                "ReleasePromoted",
                "2026-08-03T10:02:00Z",
                json!({"project_id": p, "release_id": r2}),
            ),
        ];
        let (engine, used) = replay_until(&events, Some(parse_at("2026-08-03T09:30:00Z").unwrap()));
        assert_eq!(used, 3);
        let project = engine.get_projects().pop().unwrap();
        assert_eq!(project.current_release.as_deref(), Some("r1"));
        assert_eq!(engine.get_releases().len(), 1);

        // Exactly at an event's time includes it.
        let (engine, used) = replay_until(&events, Some(parse_at("2026-08-03T10:02:00Z").unwrap()));
        assert_eq!(used, 5);
        assert_eq!(
            engine.get_projects()[0].current_release.as_deref(),
            Some("r2")
        );

        let (engine, _) = replay_until(&events, Some(parse_at("2026-08-03T08:00:00Z").unwrap()));
        assert!(engine.get_projects().is_empty());

        assert_eq!(
            window(&events, parse_at("2026-08-03T10:00:00Z").unwrap()).len(),
            2
        );
        assert_eq!(
            window(&events, parse_at("2026-08-04T00:00:00Z").unwrap()).len(),
            0
        );
    }

    #[test]
    fn reads_payload_fields() {
        let e = ev(
            "DeploymentFailed",
            "2026-08-03T09:00:00.5+00:00",
            json!({"deployment_id": "d1", "actor": "cli", "exit_code": 3, "log_tail": ["a", "b"]}),
        );
        assert_eq!(e.deployment_id(), "d1");
        assert_eq!(e.actor(), "cli");
        assert_eq!(e.i64("exit_code"), Some(3));
        assert_eq!(e.log_tail(), vec!["a".to_string(), "b".to_string()]);
        assert!(!e.bool("will_restart"));
        assert!(Ev::from_response(EventResponse {
            created_at: "not a time".into(),
            ..Default::default()
        })
        .is_none());
    }
}
