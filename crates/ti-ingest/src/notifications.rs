//! Signal K notification lifecycle documents (W10).
//!
//! Identity is vessel plus notification path plus initial raise timestamp.
//! Escalations retain that identity; clearing supplies the exclusive end time.
//! Open documents have no end and retain the point-document semantics of spec/14.

use std::collections::BTreeMap;
use std::path::Path;
use ti_contracts::{Document, Error, Result, TiConfig};
use ti_store::DocStore;

use crate::decode::{decode_delta, RawDataPoint, SignalKDelta};
use crate::normalize::is_path_allowed;

/// Durable notification documents and the current episode of each path.
pub struct NotificationDocuments {
    docs: DocStore,
    active: BTreeMap<(String, String), Document>,
}

impl NotificationDocuments {
    /// Restore active episodes after a stream reconnect or process restart.
    pub fn open(root: &Path) -> Result<Self> {
        Ok(Self::from_store(DocStore::open(root)?))
    }

    /// Construct a lifecycle tracker, including open episodes already in the store.
    pub fn from_store(docs: DocStore) -> Self {
        let mut active = BTreeMap::new();
        for doc in docs.iter().filter(|d| d.kind == "alerts" && d.ts_end.is_none() && !is_closed_point(d)) {
            if let Some(rest) = doc.id.strip_prefix("notifications/") {
                if let Some((path, _)) = rest.rsplit_once('/') {
                    active.insert((doc.vessel.clone(), path.to_owned()), doc.clone());
                }
            }
        }
        Self { docs, active }
    }

    /// Decode a recorded or live delta using the same receive-time rules as telemetry.
    pub fn ingest_message(
        &mut self,
        text: &str,
        self_urn: &str,
        receive_time: i64,
        config: &TiConfig,
    ) -> Result<()> {
        let Ok(delta) = serde_json::from_str::<SignalKDelta>(text.trim()) else {
            return Ok(());
        };
        let (points, _) = decode_delta(&delta, self_urn, receive_time);
        self.ingest(&points, config)
    }

    /// Read the current durable document set.
    pub fn documents(&self) -> &DocStore {
        &self.docs
    }

    /// Apply raw notification objects before the telemetry normalizer drops text.
    /// Sort historical rows by timestamp before invoking this method.
    pub fn ingest<'a>(
        &mut self,
        points: impl IntoIterator<Item = &'a RawDataPoint>,
        config: &TiConfig,
    ) -> Result<()> {
        let mut updates: BTreeMap<(String, String), Document> = BTreeMap::new();
        for point in points {
            if !point.path.starts_with("notifications.")
                || !is_path_allowed(&point.path, &config.allow_paths, &config.deny_paths)
            {
                continue;
            }
            let Some(state) = point.value.get("state").and_then(|v| v.as_str()) else {
                continue;
            };
            if !["normal", "alert", "warn", "alarm", "emergency"].contains(&state) {
                return Err(Error::InvalidInput(format!("notification state {state:?}")));
            }
            let vessel = if point.context.starts_with("urn:") {
                format!("vessels.{}", point.context)
            } else {
                point.context.clone()
            };
            let key = (vessel.clone(), point.path.clone());
            if state == "normal" {
                if let Some(mut doc) = self.active.remove(&key) {
                    if point.timestamp < doc.ts_start {
                        self.active.insert(key, doc);
                        continue;
                    }
                    if point.timestamp == doc.ts_start {
                        // Lead ruling: preserve a sub-second episode as a point document.
                        // The generated trailing marker distinguishes it from an open episode.
                        doc.ts_end = None;
                        doc.body.push_str(&format!("\nnotification_closed_at: {}", point.timestamp));
                    } else {
                        doc.ts_end = Some(point.timestamp);
                    }
                    updates.insert((doc.vessel.clone(), doc.id.clone()), doc);
                }
                continue;
            }
            let current = self.active.remove(&key).filter(|d| {
                d.ts_start <= point.timestamp
                    && d.ts_end.is_none_or(|end| point.timestamp < end)
            });
            let mut doc = current.unwrap_or_else(|| {
                let id = format!("notifications/{}/{}", point.path, point.timestamp);
                let prefix = format!("notifications/{}/", point.path);
                // Restore an episode during a partial replay, retaining its recorded end.
                self.docs.iter()
                    .filter(|d| !updates.contains_key(&(d.vessel.clone(), d.id.clone())))
                    .chain(updates.values()).filter(|d| {
                    d.vessel == vessel && d.id.starts_with(&prefix)
                        && d.ts_start <= point.timestamp
                        && d.ts_end.is_none_or(|end| point.timestamp < end)
                        && (!is_closed_point(d) || point.timestamp == d.ts_start)
                }).max_by_key(|d| d.ts_start).cloned().unwrap_or(Document {
                        id,
                        vessel: vessel.clone(),
                        kind: "alerts".into(),
                        ts_start: point.timestamp,
                        ts_end: None,
                        title: String::new(),
                        body: String::new(),
                    })
            });
            if point.timestamp < doc.ts_start {
                self.active.insert(key, doc);
                continue;
            }
            doc.title = format!("{} ({state})", point.path);
            let message = point.value.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let method = point.value.get("method").map(ToString::to_string).unwrap_or_default();
            doc.body = format!("{message}\nmethod: {method}\nstate: {state}");
            updates.insert((doc.vessel.clone(), doc.id.clone()), doc.clone());
            self.active.insert(key, doc);
        }
        self.docs.upsert_all(updates.into_values())
    }
}

/// A notification cleared within its start second remains a searchable point,
/// while this generated trailing marker records that it is no longer active.
pub fn is_closed_point(doc: &Document) -> bool {
    doc.id.starts_with("notifications/") && doc.ts_end.is_none()
        && doc.body.lines().last().is_some_and(|line| {
            line.strip_prefix("notification_closed_at: ")
                .is_some_and(|timestamp| timestamp.parse::<i64>().ok() == Some(doc.ts_start))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn point(ts: i64, state: &str, message: &str) -> RawDataPoint {
        RawDataPoint {
            context: "vessels.urn:mrn:signalk:uuid:test".into(),
            path: "notifications.electrical.battery".into(),
            source: "monitor".into(),
            timestamp: ts,
            value: json!({"state": state, "message": message, "method": ["visual", "sound"]}),
        }
    }

    #[test]
    fn raise_escalate_clear_and_replay_keep_one_document() {
        let config = TiConfig::default();
        let events = [
            point(1_780_000_000, "warn", "battery voltage low"),
            point(1_780_000_010, "alarm", "battery critically low"),
            point(1_780_000_030, "normal", ""),
        ];
        let mut tracker = NotificationDocuments::from_store(DocStore::in_memory());
        tracker.ingest(&events[..1], &config).unwrap();
        let raised = tracker.documents().iter().next().unwrap().clone();
        assert_eq!(raised.ts_end, None);
        tracker.ingest(&events[1..2], &config).unwrap();
        assert_eq!(tracker.documents().iter().next().unwrap().id, raised.id);
        tracker.ingest(&events[2..], &config).unwrap();
        let complete = tracker.documents().iter().next().unwrap().clone();
        assert_eq!(complete.ts_start, events[0].timestamp);
        assert_eq!(complete.ts_end, Some(events[2].timestamp));
        assert!(complete.body.contains("battery critically low"));
        assert!(complete.body.contains("sound"));
        assert!(complete.title.ends_with("(alarm)"));
        tracker.ingest(&events, &config).unwrap();
        assert_eq!(tracker.documents().len(), 1);
        assert_eq!(tracker.documents().iter().next(), Some(&complete));
    }

    #[test]
    fn equal_second_clear_keeps_point_and_restart_does_not_reopen_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = TiConfig::default();
        let start = 1_780_000_000;
        let mut tracker = NotificationDocuments::open(dir.path()).unwrap();
        let events = [point(start, "alarm", "battery critical"), point(start, "normal", "")];
        tracker.ingest(&events, &config).unwrap();
        let first = tracker.documents().iter().next().unwrap().clone();
        assert!(is_closed_point(&first));
        drop(tracker);
        let mut tracker = NotificationDocuments::open(dir.path()).unwrap();
        tracker.ingest(&events, &config).unwrap();
        assert_eq!(tracker.documents().iter().next(), Some(&first));
        tracker.ingest([&point(start + 10, "warn", "battery low again")], &config).unwrap();
        assert_eq!(tracker.documents().len(), 2);
    }

    #[test]
    fn restart_keeps_raise_id_and_second_episode_is_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let config = TiConfig::default();
        let mut tracker = NotificationDocuments::open(dir.path()).unwrap();
        tracker.ingest([&point(1_780_000_000, "warn", "battery low")], &config).unwrap();
        drop(tracker);
        let mut tracker = NotificationDocuments::open(dir.path()).unwrap();
        tracker.ingest([
            &point(1_780_000_010, "alarm", "battery critical"),
            &point(1_780_000_020, "normal", ""),
            &point(1_780_000_030, "warn", "battery low again"),
        ], &config).unwrap();
        assert_eq!(tracker.documents().len(), 2);
        let docs: Vec<_> = tracker.documents().iter().collect();
        assert_eq!(docs[0].ts_end, Some(1_780_000_020));
        assert_eq!(docs[1].ts_end, None);
    }
}
