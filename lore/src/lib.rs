//! Phase 6: the lore engine. Every generated feature has a stable id
//! derived from (seed, stage, location); the first time one is viewed, the
//! engine assembles its deterministic context from the generators and asks
//! Claude to write its entry — top-down, realm chronicle before town entry,
//! so fiction nests the way terrain does. Results are cached in SQLite keyed
//! by (seed, generator version, id): the cache is the canon. The world works
//! fully offline with lore disabled — this layer is additive.

mod client;
pub mod context;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use world_gen::Planet;

use context::{feature_name, parse_id, prompt_for, realm_of, FeatureRef, SYSTEM_PROMPT};

/// Writes one entry given (system prompt, user prompt). The real one calls
/// the Claude API; tests inject their own.
pub type Writer = Arc<dyn Fn(&str, &str) -> Result<String, String> + Send + Sync>;

pub enum LoreStatus {
    Ready {
        name: String,
        body: String,
        realm: Option<(String, String)>,
    },
    Generating,
    Disabled {
        hint: String,
    },
    NotFound,
    Failed {
        message: String,
    },
}

pub struct LoreEngine {
    db: Mutex<Connection>,
    seed: u64,
    genver: u32,
    writer: Option<Writer>,
    pub model: String,
    in_flight: Mutex<HashSet<String>>,
    errors: Mutex<HashMap<String, String>>,
}

impl LoreEngine {
    /// Open (creating if needed) the canon cache and resolve API credentials
    /// from the environment. Missing credentials disable generation but not
    /// the cache — already-written canon still serves.
    pub fn open(db_path: &Path, seed: u64, genver: u32) -> Result<Self, String> {
        let db = Connection::open(db_path).map_err(|e| e.to_string())?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS lore (
                seed INTEGER NOT NULL,
                genver INTEGER NOT NULL,
                id TEXT NOT NULL,
                name TEXT NOT NULL,
                body TEXT NOT NULL,
                model TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (seed, genver, id)
            )",
        )
        .map_err(|e| e.to_string())?;

        let model = std::env::var("LORE_MODEL").unwrap_or_else(|_| "claude-opus-4-8".into());
        let writer = client::api_writer(model.clone());
        Ok(Self {
            db: Mutex::new(db),
            seed,
            genver,
            writer,
            model,
            in_flight: Mutex::new(HashSet::new()),
            errors: Mutex::new(HashMap::new()),
        })
    }

    /// Test/offline constructor with an injected writer.
    pub fn with_writer(
        db_path: &Path,
        seed: u64,
        genver: u32,
        writer: Writer,
    ) -> Result<Self, String> {
        let mut engine = Self::open(db_path, seed, genver)?;
        engine.writer = Some(writer);
        engine.model = "injected".into();
        Ok(engine)
    }

    pub fn enabled(&self) -> bool {
        self.writer.is_some()
    }

    /// Look up a feature's lore; if it isn't written yet, start writing it
    /// in the background and report `Generating`.
    pub fn request(self: &Arc<Self>, planet: &Arc<Planet>, id: &str) -> LoreStatus {
        let Some(fref) = parse_id(planet, id) else {
            return LoreStatus::NotFound;
        };
        let realm = match fref {
            FeatureRef::Settlement(_) => Some(realm_of(planet, fref)),
            FeatureRef::Realm(_) => None,
        };

        if let Some(body) = self.cached(id) {
            return LoreStatus::Ready {
                name: feature_name(planet, fref),
                body,
                realm,
            };
        }
        if self.writer.is_none() {
            return LoreStatus::Disabled {
                hint: "Lore is offline: set ANTHROPIC_API_KEY (or log in with \
                       `ant auth login`) and restart the server."
                    .into(),
            };
        }
        // A failed attempt reports once, then clears so the next click retries.
        if let Some(message) = self.errors.lock().unwrap().remove(id) {
            return LoreStatus::Failed { message };
        }
        if !self.begin(id) {
            return LoreStatus::Generating;
        }

        let engine = self.clone();
        let planet = planet.clone();
        let id = id.to_string();
        std::thread::spawn(move || {
            let outcome = engine.generate(&planet, &id, fref);
            if let Err(e) = outcome {
                engine.errors.lock().unwrap().insert(id.clone(), e);
            }
            engine.in_flight.lock().unwrap().remove(&id);
        });
        LoreStatus::Generating
    }

    /// Write one feature's entry (realm chronicle first for settlements).
    fn generate(&self, planet: &Planet, id: &str, fref: FeatureRef) -> Result<(), String> {
        let realm_body = match fref {
            FeatureRef::Settlement(_) => {
                let (realm_id, _) = realm_of(planet, fref);
                Some(self.ensure(planet, &realm_id)?)
            }
            FeatureRef::Realm(_) => None,
        };
        let writer = self.writer.as_ref().ok_or("lore disabled")?;
        let prompt = prompt_for(planet, fref, realm_body.as_deref());
        let body = writer(SYSTEM_PROMPT, &prompt)?;
        self.store(id, &feature_name(planet, fref), &body);
        Ok(())
    }

    /// Get a feature's lore, generating it inline if needed — used for the
    /// realm-before-settlement dependency. If another thread is already
    /// writing it, wait for that instead of writing it twice.
    fn ensure(&self, planet: &Planet, id: &str) -> Result<String, String> {
        if let Some(body) = self.cached(id) {
            return Ok(body);
        }
        if self.begin(id) {
            let fref = parse_id(planet, id).ok_or("bad dependency id")?;
            let outcome = self.generate(planet, id, fref);
            self.in_flight.lock().unwrap().remove(id);
            outcome?;
            return self.cached(id).ok_or_else(|| "store failed".into());
        }
        // Someone else is writing it; wait politely.
        for _ in 0..240 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if let Some(body) = self.cached(id) {
                return Ok(body);
            }
            if !self.in_flight.lock().unwrap().contains(id) {
                break;
            }
        }
        Err("dependency generation did not complete".into())
    }

    fn begin(&self, id: &str) -> bool {
        self.in_flight.lock().unwrap().insert(id.to_string())
    }

    fn cached(&self, id: &str) -> Option<String> {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT body FROM lore WHERE seed = ?1 AND genver = ?2 AND id = ?3",
                rusqlite::params![self.seed as i64, self.genver, id],
                |row| row.get(0),
            )
            .ok()
    }

    fn store(&self, id: &str, name: &str, body: &str) {
        let _ = self.db.lock().unwrap().execute(
            "INSERT OR IGNORE INTO lore (seed, genver, id, name, body, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![self.seed as i64, self.genver, id, name, body, self.model],
        );
    }

    pub fn entries_written(&self) -> u32 {
        self.db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM lore WHERE seed = ?1 AND genver = ?2",
                rusqlite::params![self.seed as i64, self.genver],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fractal-lore-test-{tag}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn planet() -> Arc<Planet> {
        Arc::new(Planet::new(42))
    }

    #[test]
    fn context_is_deterministic_and_grounded() {
        let planet = planet();
        let civ = planet.civilization();
        let i = civ.settlements.iter().position(|s| s.capital).unwrap();
        let fref = FeatureRef::Settlement(i);
        let a = prompt_for(&planet, fref, None);
        let b = prompt_for(&planet, fref, None);
        assert_eq!(a, b, "context must be a pure function of the world");
        let s = &civ.settlements[i];
        assert!(a.contains(&s.name));
        assert!(a.contains("people"), "population must be in the brief");
        assert!(a.contains("Life expectancy"), "era demographics missing");
    }

    #[test]
    fn realm_is_written_before_its_settlement_and_canon_sticks() {
        let planet = planet();
        let civ = planet.civilization();
        // A non-capital settlement, so the realm dependency actually fires.
        let s = civ.settlements.iter().find(|s| !s.capital).unwrap();
        let id = format!("s{}", s.cell);

        let order = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicU32::new(0));
        let writer: Writer = {
            let order = order.clone();
            let calls = calls.clone();
            Arc::new(move |_sys, prompt| {
                calls.fetch_add(1, Ordering::SeqCst);
                order.lock().unwrap().push(prompt.to_string());
                Ok(format!("Entry #{}", calls.load(Ordering::SeqCst)))
            })
        };

        let db = temp_db("order");
        let engine =
            Arc::new(LoreEngine::with_writer(&db, planet.seed, 9, writer).unwrap());

        assert!(matches!(
            engine.request(&planet, &id),
            LoreStatus::Generating
        ));
        // Wait for the background thread to finish both entries.
        for _ in 0..100 {
            if matches!(engine.request(&planet, &id), LoreStatus::Ready { .. }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let status = engine.request(&planet, &id);
        let LoreStatus::Ready { body, realm, .. } = status else {
            panic!("lore never became ready");
        };
        assert_eq!(body, "Entry #2", "settlement written after its realm");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let prompts = order.lock().unwrap();
        assert!(prompts[0].contains("chronicle"), "realm first");
        assert!(prompts[0].contains("The annals:"), "realm prompt carries the annals");
        assert!(prompts[1].contains("atlas entry"));
        assert!(
            prompts[1].contains("Entry #1"),
            "settlement prompt must carry the realm chronicle"
        );

        // Canon sticks: a fresh engine on the same db serves without writing.
        let (realm_id, _) = realm.expect("settlement has a realm");
        let boom: Writer = Arc::new(|_, _| panic!("must not rewrite canon"));
        let engine2 =
            Arc::new(LoreEngine::with_writer(&db, planet.seed, 9, boom).unwrap());
        assert!(matches!(
            engine2.request(&planet, &id),
            LoreStatus::Ready { .. }
        ));
        assert!(matches!(
            engine2.request(&planet, &realm_id),
            LoreStatus::Ready { .. }
        ));
        assert_eq!(engine2.entries_written(), 2);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn unknown_ids_and_failures_are_reported() {
        let planet = planet();
        let db = temp_db("errors");
        let failing: Writer = Arc::new(|_, _| Err("the courier was eaten".into()));
        let engine =
            Arc::new(LoreEngine::with_writer(&db, planet.seed, 9, failing).unwrap());

        assert!(matches!(
            engine.request(&planet, "s999999999"),
            LoreStatus::NotFound
        ));
        assert!(matches!(
            engine.request(&planet, "x1"),
            LoreStatus::NotFound
        ));

        let s = &planet.civilization().settlements[0];
        let id = format!("s{}", s.cell);
        engine.request(&planet, &id);
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if !engine.in_flight.lock().unwrap().contains(&id) {
                break;
            }
        }
        assert!(matches!(
            engine.request(&planet, &id),
            LoreStatus::Failed { .. }
        ));
        // And the failure cleared, so the next request retries.
        assert!(matches!(
            engine.request(&planet, &id),
            LoreStatus::Generating
        ));
        let _ = std::fs::remove_file(&db);
    }
}
