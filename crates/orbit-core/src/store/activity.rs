//! Local response measurements. Deliberately independent of session and project lifetimes.
use super::{Connection, Session, Store, now};
use crate::provider::Response;
use anyhow::Result;
use rusqlite::params;
use std::collections::BTreeMap;

const DAY: u64 = 86_400;
pub const DAYS: usize = 30;
const WINDOW: u64 = DAYS as u64 * DAY;

pub(super) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS response_activity (
        completed INTEGER NOT NULL, model TEXT NOT NULL, fast INTEGER NOT NULL,
        input INTEGER NOT NULL, output INTEGER NOT NULL, cached INTEGER NOT NULL,
        stream_seconds REAL NOT NULL, latency_seconds REAL
    );
    CREATE INDEX IF NOT EXISTS response_activity_time ON response_activity(completed);",
    )?;
    prune(db, now())
}

pub(super) fn prune(db: &Connection, at: u64) -> Result<()> {
    db.execute(
        "DELETE FROM response_activity WHERE completed <= ?1",
        [at as i64 - WINDOW as i64],
    )?;
    Ok(())
}

pub(crate) struct Sample {
    completed: u64,
    model: String,
    fast: bool,
    input: u64,
    output: u64,
    cached: u64,
    stream_seconds: f64,
    latency_seconds: Option<f64>,
}

impl Sample {
    pub(crate) fn new(session: &Session, response: &Response) -> Self {
        Self {
            completed: now(),
            model: session.model.clone(),
            fast: session.fast,
            input: response.usage.input,
            output: response.usage.output,
            cached: response.usage.cached,
            stream_seconds: response.streaming_time.as_secs_f64(),
            latency_seconds: response.first_output_latency.map(|d| d.as_secs_f64()),
        }
    }

    pub(super) fn insert(&self, db: &Connection) -> Result<()> {
        db.execute(
            "INSERT INTO response_activity
            (completed, model, fast, input, output, cached, stream_seconds, latency_seconds)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                self.completed,
                self.model,
                self.fast,
                self.input,
                self.output,
                self.cached,
                self.stream_seconds,
                self.latency_seconds
            ],
        )?;
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output
    }
}

#[derive(Debug, Default)]
pub struct Day {
    pub total: u64,
    pub models: BTreeMap<String, u64>,
}

#[derive(Debug)]
pub struct Generation {
    pub model: String,
    pub fast: bool,
    pub tokens_per_second: Option<f64>,
    pub latency_seconds: Option<f64>,
}

#[derive(Debug, Default)]
pub struct Summary {
    /// Rolling 1-, 7-, and 30-day windows, in that order.
    pub tokens: [Tokens; 3],
    /// Oldest first; the final bucket is the same interval as the 1-day total.
    pub days: [Day; DAYS],
    pub generation: Vec<Generation>,
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        values[mid - 1].midpoint(values[mid])
    } else {
        values[mid]
    })
}

impl Store {
    pub fn subscribe_activity(&self) -> tokio::sync::watch::Receiver<()> {
        self.activity_changes.subscribe()
    }

    pub fn prune_activity(&self) -> Result<()> {
        self.prune_activity_at(now())
    }

    fn prune_activity_at(&self, at: u64) -> Result<()> {
        // Most ticks have nothing to delete. Do not claim the single writer.
        if self.read_connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM response_activity WHERE completed <= ?1)",
            [at as i64 - WINDOW as i64],
            |r| r.get::<_, bool>(0),
        )? {
            prune(&self.db.lock().expect("database lock"), at)?;
        }
        Ok(())
    }

    pub fn activity_summary(&self) -> Result<Summary> {
        self.activity_at(now())
    }

    fn activity_at(&self, at: u64) -> Result<Summary> {
        let mut summary = Summary::default();
        let mut samples = BTreeMap::<(String, bool), (Vec<f64>, Vec<f64>)>::new();
        {
            let db = self.read_connection()?;
            let mut stmt = db.prepare("SELECT completed, model, fast, input, output, cached, stream_seconds, latency_seconds
                FROM response_activity WHERE completed > ?1 AND completed <= ?2")?;
            let mut rows = stmt.query(params![at as i64 - WINDOW as i64, at])?;
            while let Some(row) = rows.next()? {
                let age = ((at - row.get::<_, u64>(0)?) / DAY) as usize;
                let model: String = row.get(1)?;
                let input: u64 = row.get(3)?;
                let output: u64 = row.get(4)?;
                let cached: u64 = row.get(5)?;
                for (tokens, window) in summary.tokens.iter_mut().zip([1, 7, DAYS]) {
                    if age < window {
                        tokens.input += input;
                        tokens.output += output;
                        tokens.cached += cached;
                    }
                }
                let day = &mut summary.days[DAYS - 1 - age];
                day.total += input + output;
                *day.models.entry(model.clone()).or_default() += input + output;
                if age < 7 {
                    let (rates, latencies) = samples.entry((model, row.get(2)?)).or_default();
                    let stream: f64 = row.get(6)?;
                    if stream > 0. && output > 0 {
                        rates.push(output as f64 / stream);
                    }
                    latencies.extend(row.get::<_, Option<f64>>(7)?);
                }
            }
        }
        summary.generation = samples
            .into_iter()
            .map(|((model, fast), (rates, latencies))| Generation {
                model,
                fast,
                tokens_per_second: median(rates),
                latency_seconds: median(latencies),
            })
            .collect();
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_windows_model_breakdowns_and_per_response_medians() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let at = 100 * DAY;
        let add = |age, model: &str, fast, input, output, cached, stream, latency| {
            Sample {
                completed: at - age,
                model: model.into(),
                fast,
                input,
                output,
                cached,
                stream_seconds: stream,
                latency_seconds: latency,
            }
            .insert(&store.db.lock().unwrap())
            .unwrap();
        };
        // Reads exclude expired rows even before the periodic deletion.
        add(0, "alpha", false, 90, 18, 45, 2., Some(1.));
        add(DAY - 1, "beta", false, 5, 7, 0, 0., None);
        add(DAY, "alpha", true, 71, 20, 40, 0.5, Some(0.2));
        add(7 * DAY - 1, "alpha", false, 8, 30, 1, 1., Some(2.));
        add(7 * DAY, "alpha", false, 9, 10, 4, 0.1, Some(0.01));
        add(30 * DAY - 1, "beta", false, 24, 2, 0, 0., None);
        add(30 * DAY, "expired", true, 1000, 999, 100, 1., None);
        let s = store.activity_at(at).unwrap();
        assert_eq!(
            s.tokens.map(|t| (t.input, t.output, t.cached)),
            [(95, 25, 45), (174, 75, 86), (207, 87, 90),]
        );
        assert_eq!(s.days[0].total, 26);
        assert_eq!(
            s.days[29].models,
            BTreeMap::from([("alpha".into(), 108), ("beta".into(), 12)])
        );
        assert_eq!(s.days.iter().map(|d| d.total).sum::<u64>(), 294);
        assert_eq!(
            s.generation
                .iter()
                .map(|g| (
                    g.model.as_str(),
                    g.fast,
                    g.tokens_per_second,
                    g.latency_seconds
                ))
                .collect::<Vec<_>>(),
            [
                ("alpha", false, Some(19.5), Some(1.5)),
                ("alpha", true, Some(40.), Some(0.2)),
                ("beta", false, None, None),
            ]
        );
        store.prune_activity_at(at).unwrap();
        // Move the reporting clock back: a filtered-but-not-deleted row would reappear.
        assert!(
            !store
                .activity_at(at - DAY)
                .unwrap()
                .days
                .iter()
                .any(|d| d.models.contains_key("expired"))
        );
    }
}
