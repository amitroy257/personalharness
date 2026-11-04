//! Local SQLite store: conversations, per-request usage metadata, ratings and
//! caches. API-server requests are logged as metadata only (no prompt text).

use std::collections::HashMap;
use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::util::now_ms;

pub struct Store {
    conn: Mutex<Connection>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UsageRecord {
    pub provider: String,
    pub model: String,
    pub family: String,
    pub source: String,
    pub ok: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub ttft_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FamilyStats {
    pub family: String,
    pub attempts: i64,
    pub successes: i64,
    pub rate_limited: i64,
    /// Average output tokens/second over successful calls with timing data.
    pub tokens_per_sec: Option<f64>,
    pub avg_ttft_ms: Option<f64>,
    pub completion_tokens: i64,
    pub thumbs_up: i64,
    pub thumbs_down: i64,
    pub last_used: Option<i64>,
    /// Successful calls in the last 7 days vs the 7 days before.
    pub recent_uses: i64,
    pub prior_uses: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub model: String,
    pub system_prompt: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub reasoning: String,
    pub api_id: String,
    pub family: String,
    pub created_at: i64,
    pub rating: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub duration_ms: Option<i64>,
    pub notes: String,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
-- overwrite deleted content with zeros instead of leaving it in free pages
PRAGMA secure_delete = ON;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS usage (
  id INTEGER PRIMARY KEY,
  ts INTEGER NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  family TEXT NOT NULL,
  source TEXT NOT NULL,
  ok INTEGER NOT NULL,
  status INTEGER,
  error TEXT,
  ttft_ms INTEGER,
  duration_ms INTEGER,
  prompt_tokens INTEGER,
  completion_tokens INTEGER
);
CREATE INDEX IF NOT EXISTS usage_ts ON usage(ts);
CREATE INDEX IF NOT EXISTS usage_family ON usage(family);
CREATE TABLE IF NOT EXISTS conversations (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  model TEXT NOT NULL,
  system_prompt TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  reasoning TEXT NOT NULL DEFAULT '',
  api_id TEXT NOT NULL DEFAULT '',
  family TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  rating INTEGER NOT NULL DEFAULT 0,
  prompt_tokens INTEGER,
  completion_tokens INTEGER,
  duration_ms INTEGER,
  notes TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS messages_conv ON messages(conversation_id, created_at);
CREATE TABLE IF NOT EXISTS kv (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
"#;

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        #[cfg(unix)]
        {
            // Create the database owner-only *before* SQLite opens it: SQLite
            // gives the -wal/-shm side files the same mode as the database file.
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let _ = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path);
            for suffix in ["", "-wal", "-shm", "-journal"] {
                let p = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
                if p.exists() {
                    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
                }
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    // ---- usage -------------------------------------------------------------

    pub fn record_usage(&self, r: &UsageRecord) {
        let c = self.conn.lock();
        let res = c.execute(
            "INSERT INTO usage (ts, provider, model, family, source, ok, status, error, ttft_ms, duration_ms, prompt_tokens, completion_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                now_ms(),
                r.provider,
                r.model,
                r.family,
                r.source,
                r.ok as i64,
                r.status.map(|s| s as i64),
                r.error.as_deref().map(|e| crate::util::truncate_chars(e, 500)),
                r.ttft_ms,
                r.duration_ms,
                r.prompt_tokens,
                r.completion_tokens
            ],
        );
        if let Err(e) = res {
            tracing::warn!("record_usage failed: {e}");
        }
    }

    /// Aggregated stats per model family over the last `days` days.
    pub fn family_stats(&self, days: i64) -> HashMap<String, FamilyStats> {
        let now = now_ms();
        let since = now - days * 86_400_000;
        let week = now - 7 * 86_400_000;
        let two_weeks = now - 14 * 86_400_000;
        let c = self.conn.lock();
        let mut out: HashMap<String, FamilyStats> = HashMap::new();
        let mut stmt = c
            .prepare(
                "SELECT family,
                        COUNT(*),
                        SUM(ok),
                        SUM(CASE WHEN status = 429 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN ok = 1 AND completion_tokens > 0 AND duration_ms > COALESCE(ttft_ms, 0) + 50
                                 THEN completion_tokens ELSE 0 END),
                        SUM(CASE WHEN ok = 1 AND completion_tokens > 0 AND duration_ms > COALESCE(ttft_ms, 0) + 50
                                 THEN duration_ms - COALESCE(ttft_ms, 0) ELSE 0 END),
                        AVG(CASE WHEN ok = 1 THEN ttft_ms END),
                        SUM(CASE WHEN ok = 1 THEN COALESCE(completion_tokens, 0) ELSE 0 END),
                        MAX(ts),
                        SUM(CASE WHEN ok = 1 AND ts >= ?2 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN ok = 1 AND ts >= ?3 AND ts < ?2 THEN 1 ELSE 0 END)
                 FROM usage WHERE ts >= ?1 GROUP BY family",
            )
            .expect("prepare stats");
        let rows = stmt
            .query_map(params![since, week, two_weeks], |r| {
                let tok: i64 = r.get::<_, Option<i64>>(4)?.unwrap_or(0);
                let ms: i64 = r.get::<_, Option<i64>>(5)?.unwrap_or(0);
                Ok(FamilyStats {
                    family: r.get(0)?,
                    attempts: r.get(1)?,
                    successes: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    rate_limited: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    tokens_per_sec: if tok > 0 && ms > 0 { Some(tok as f64 / (ms as f64 / 1000.0)) } else { None },
                    avg_ttft_ms: r.get(6)?,
                    completion_tokens: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    thumbs_up: 0,
                    thumbs_down: 0,
                    last_used: r.get(8)?,
                    recent_uses: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                    prior_uses: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                })
            })
            .expect("query stats");
        for row in rows.flatten() {
            out.insert(row.family.clone(), row);
        }
        drop(stmt);
        let mut stmt = c
            .prepare(
                "SELECT family, SUM(CASE WHEN rating > 0 THEN 1 ELSE 0 END), SUM(CASE WHEN rating < 0 THEN 1 ELSE 0 END)
                 FROM messages WHERE role = 'assistant' AND rating != 0 AND family != '' GROUP BY family",
            )
            .expect("prepare ratings");
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
            .expect("query ratings");
        for (fam, up, down) in rows.flatten() {
            let e = out.entry(fam.clone()).or_insert_with(|| FamilyStats { family: fam, ..Default::default() });
            e.thumbs_up = up;
            e.thumbs_down = down;
        }
        out
    }

    /// Per `provider/model` success ratio and speed, used to order providers
    /// serving the same model.
    pub fn route_stats(&self, days: i64) -> HashMap<String, (i64, i64, Option<f64>)> {
        let since = now_ms() - days * 86_400_000;
        let c = self.conn.lock();
        let mut stmt = c
            .prepare(
                "SELECT provider || '/' || model, COUNT(*), SUM(ok),
                        SUM(CASE WHEN ok = 1 AND completion_tokens > 0 AND duration_ms > COALESCE(ttft_ms,0) + 50 THEN completion_tokens ELSE 0 END),
                        SUM(CASE WHEN ok = 1 AND completion_tokens > 0 AND duration_ms > COALESCE(ttft_ms,0) + 50 THEN duration_ms - COALESCE(ttft_ms,0) ELSE 0 END)
                 FROM usage WHERE ts >= ?1 GROUP BY provider, model",
            )
            .expect("prepare route stats");
        let rows = stmt
            .query_map(params![since], |r| {
                let tok: i64 = r.get::<_, Option<i64>>(3)?.unwrap_or(0);
                let ms: i64 = r.get::<_, Option<i64>>(4)?.unwrap_or(0);
                Ok((
                    r.get::<_, String>(0)?,
                    (
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        if tok > 0 && ms > 0 { Some(tok as f64 / (ms as f64 / 1000.0)) } else { None },
                    ),
                ))
            })
            .expect("query route stats");
        rows.flatten().collect()
    }

    pub fn usage_totals(&self, days: i64) -> serde_json::Value {
        let since = now_ms() - days * 86_400_000;
        let c = self.conn.lock();
        let (n, ok, toks): (i64, i64, i64) = c
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(ok),0), COALESCE(SUM(COALESCE(prompt_tokens,0) + COALESCE(completion_tokens,0)),0) FROM usage WHERE ts >= ?1",
                params![since],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap_or((0, 0, 0));
        let mut stmt = c
            .prepare("SELECT provider, COUNT(*), COALESCE(SUM(ok),0) FROM usage WHERE ts >= ?1 GROUP BY provider ORDER BY 2 DESC")
            .expect("prepare");
        let by_provider: Vec<serde_json::Value> = stmt
            .query_map(params![since], |r| {
                Ok(serde_json::json!({"provider": r.get::<_, String>(0)?, "requests": r.get::<_, i64>(1)?, "ok": r.get::<_, i64>(2)?}))
            })
            .expect("query")
            .flatten()
            .collect();
        serde_json::json!({ "days": days, "requests": n, "ok": ok, "tokens": toks, "by_provider": by_provider })
    }

    // ---- conversations -----------------------------------------------------

    pub fn create_conversation(&self, title: &str, model: &str, system_prompt: &str) -> Conversation {
        let now = now_ms();
        let conv = Conversation {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.to_string(),
            model: model.to_string(),
            system_prompt: system_prompt.to_string(),
            created_at: now,
            updated_at: now,
        };
        let c = self.conn.lock();
        let _ = c.execute(
            "INSERT INTO conversations (id, title, model, system_prompt, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![conv.id, conv.title, conv.model, conv.system_prompt, now, now],
        );
        conv
    }

    pub fn get_conversation(&self, id: &str) -> Option<Conversation> {
        let c = self.conn.lock();
        c.query_row(
            "SELECT id, title, model, system_prompt, created_at, updated_at FROM conversations WHERE id = ?1",
            params![id],
            |r| {
                Ok(Conversation {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    model: r.get(2)?,
                    system_prompt: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn list_conversations(&self, limit: i64) -> Vec<Conversation> {
        let c = self.conn.lock();
        let mut stmt = c
            .prepare(
                "SELECT id, title, model, system_prompt, created_at, updated_at FROM conversations ORDER BY updated_at DESC LIMIT ?1",
            )
            .expect("prepare");
        stmt.query_map(params![limit], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                title: r.get(1)?,
                model: r.get(2)?,
                system_prompt: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        })
        .expect("query")
        .flatten()
        .collect()
    }

    pub fn update_conversation(&self, id: &str, title: Option<&str>, model: Option<&str>, system_prompt: Option<&str>) {
        let c = self.conn.lock();
        let now = now_ms();
        if let Some(t) = title {
            let _ = c.execute("UPDATE conversations SET title = ?2 WHERE id = ?1", params![id, t]);
        }
        if let Some(m) = model {
            let _ = c.execute("UPDATE conversations SET model = ?2 WHERE id = ?1", params![id, m]);
        }
        if let Some(s) = system_prompt {
            let _ = c.execute("UPDATE conversations SET system_prompt = ?2 WHERE id = ?1", params![id, s]);
        }
        let _ = c.execute("UPDATE conversations SET updated_at = ?2 WHERE id = ?1", params![id, now]);
    }

    pub fn delete_conversation(&self, id: &str) {
        let c = self.conn.lock();
        let _ = c.execute("DELETE FROM messages WHERE conversation_id = ?1", params![id]);
        let _ = c.execute("DELETE FROM conversations WHERE id = ?1", params![id]);
        // Deleted text must not survive in the write-ahead log either.
        let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_message(
        &self,
        conversation_id: &str,
        role: &str,
        content: &str,
        reasoning: &str,
        api_id: &str,
        family: &str,
        tokens: (Option<i64>, Option<i64>),
        duration_ms: Option<i64>,
        notes: &str,
    ) -> Message {
        let m = Message {
            id: uuid::Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            reasoning: reasoning.to_string(),
            api_id: api_id.to_string(),
            family: family.to_string(),
            created_at: now_ms(),
            rating: 0,
            prompt_tokens: tokens.0,
            completion_tokens: tokens.1,
            duration_ms,
            notes: notes.to_string(),
        };
        let c = self.conn.lock();
        let _ = c.execute(
            "INSERT INTO messages (id, conversation_id, role, content, reasoning, api_id, family, created_at, rating, prompt_tokens, completion_tokens, duration_ms, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11, ?12)",
            params![
                m.id,
                m.conversation_id,
                m.role,
                m.content,
                m.reasoning,
                m.api_id,
                m.family,
                m.created_at,
                m.prompt_tokens,
                m.completion_tokens,
                m.duration_ms,
                m.notes
            ],
        );
        let _ = c.execute("UPDATE conversations SET updated_at = ?2 WHERE id = ?1", params![conversation_id, m.created_at]);
        m
    }

    pub fn messages(&self, conversation_id: &str) -> Vec<Message> {
        let c = self.conn.lock();
        let mut stmt = c
            .prepare(
                "SELECT id, conversation_id, role, content, reasoning, api_id, family, created_at, rating, prompt_tokens, completion_tokens, duration_ms, notes
                 FROM messages WHERE conversation_id = ?1 ORDER BY created_at ASC, rowid ASC",
            )
            .expect("prepare");
        stmt.query_map(params![conversation_id], |r| {
            Ok(Message {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                reasoning: r.get(4)?,
                api_id: r.get(5)?,
                family: r.get(6)?,
                created_at: r.get(7)?,
                rating: r.get(8)?,
                prompt_tokens: r.get(9)?,
                completion_tokens: r.get(10)?,
                duration_ms: r.get(11)?,
                notes: r.get(12)?,
            })
        })
        .expect("query")
        .flatten()
        .collect()
    }

    pub fn rate_message(&self, id: &str, rating: i64) -> bool {
        let c = self.conn.lock();
        c.execute("UPDATE messages SET rating = ?2 WHERE id = ?1 AND role = 'assistant'", params![id, rating.clamp(-1, 1)])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    // ---- kv cache ----------------------------------------------------------

    pub fn kv_get(&self, key: &str) -> Option<(String, i64)> {
        let c = self.conn.lock();
        c.query_row("SELECT value, updated_at FROM kv WHERE key = ?1", params![key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .ok()
            .flatten()
    }

    pub fn kv_set(&self, key: &str, value: &str) {
        let c = self.conn.lock();
        let _ = c.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now_ms()],
        );
    }

    // ---- wiping ------------------------------------------------------------

    pub fn wipe(&self, what: &str) -> anyhow::Result<()> {
        let c = self.conn.lock();
        match what {
            "chats" => c.execute_batch("DELETE FROM messages; DELETE FROM conversations;")?,
            "usage" => c.execute_batch("DELETE FROM usage; UPDATE messages SET rating = 0;")?,
            "cache" => c.execute_batch("DELETE FROM kv;")?,
            "all" => c.execute_batch("DELETE FROM messages; DELETE FROM conversations; DELETE FROM usage; DELETE FROM kv;")?,
            other => anyhow::bail!("unknown wipe target: {other}"),
        }
        // secure_delete zeroes freed pages; VACUUM rebuilds the file; the
        // TRUNCATE checkpoint empties the WAL, which still holds old page images.
        c.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files_containing(dir: &Path, needle: &str) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|f| {
                let p = f.ok()?.path();
                let bytes = std::fs::read(&p).ok()?;
                let name = p.file_name()?.to_string_lossy().to_string();
                String::from_utf8_lossy(&bytes).contains(needle).then_some(name)
            })
            .collect()
    }

    /// Regression (SQA F1): deleted chat text was recoverable from harness.db-wal.
    #[test]
    fn deleted_chats_are_not_recoverable_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("harness.db")).unwrap();
        let keep = s.create_conversation("keep", "auto", "");
        s.add_message(&keep.id, "user", "KEEP-MARKER-1", "", "", "", (None, None), None, "");
        let gone = s.create_conversation("gone", "auto", "");
        for _ in 0..30 {
            s.add_message(&gone.id, "user", "DELETE-MARKER-2 my password is hunter2", "", "", "", (None, None), None, "");
        }
        s.delete_conversation(&gone.id);
        assert_eq!(files_containing(dir.path(), "DELETE-MARKER-2"), Vec::<String>::new(), "single chat delete");
        assert!(!files_containing(dir.path(), "KEEP-MARKER-1").is_empty(), "kept chat still stored");
        s.wipe("chats").unwrap();
        assert_eq!(files_containing(dir.path(), "KEEP-MARKER-1"), Vec::<String>::new(), "wipe all chats");
    }

    /// Regression (SQA F2): -wal/-shm side files were created world-readable (0644).
    #[cfg(unix)]
    #[test]
    fn database_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("harness.db")).unwrap();
        s.record_usage(&UsageRecord { provider: "p".into(), model: "m".into(), family: "f".into(), ok: true, ..Default::default() });
        for f in std::fs::read_dir(dir.path()).unwrap() {
            let p = f.unwrap().path();
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{} is {mode:o}", p.display());
        }
    }

    #[test]
    fn usage_and_ratings_aggregate() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..4 {
            s.record_usage(&UsageRecord {
                provider: "groq".into(),
                model: "llama-3.3-70b-versatile".into(),
                family: "llama-3.3-70b".into(),
                source: "api".into(),
                ok: i != 3,
                status: Some(if i == 3 { 429 } else { 200 }),
                ttft_ms: Some(100),
                duration_ms: Some(1100),
                completion_tokens: Some(500),
                ..Default::default()
            });
        }
        let conv = s.create_conversation("t", "auto", "");
        let m = s.add_message(
            &conv.id,
            "assistant",
            "hi",
            "",
            "groq/llama-3.3-70b-versatile",
            "llama-3.3-70b",
            (None, None),
            None,
            "",
        );
        assert!(s.rate_message(&m.id, 1));
        let st = s.family_stats(30);
        let f = &st["llama-3.3-70b"];
        assert_eq!(f.attempts, 4);
        assert_eq!(f.successes, 3);
        assert_eq!(f.rate_limited, 1);
        assert_eq!(f.thumbs_up, 1);
        assert_eq!(f.recent_uses, 3);
        // 4 rows have 500 tokens over 1000ms but only ok rows count => 1500 tok / 3s
        assert!((f.tokens_per_sec.unwrap() - 500.0).abs() < 1e-6);
        let rs = s.route_stats(30);
        assert_eq!(rs["groq/llama-3.3-70b-versatile"].0, 4);
        assert_eq!(s.messages(&conv.id).len(), 1);
        s.delete_conversation(&conv.id);
        assert!(s.list_conversations(10).is_empty());
        s.wipe("all").unwrap();
        assert!(s.family_stats(30).is_empty());
    }
}
