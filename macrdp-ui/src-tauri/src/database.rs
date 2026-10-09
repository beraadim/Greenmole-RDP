use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

/// SQLite database for storing connection history and traffic statistics.
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    pub fn new(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path)?;
        }
        macrdp_core::credentials::protect_config_file(path)?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        // Create tables
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS connections (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_ip TEXT NOT NULL,
                client_name TEXT NOT NULL DEFAULT '',
                connected_at TEXT NOT NULL,
                disconnected_at TEXT,
                duration_secs INTEGER DEFAULT 0,
                bytes_total INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS traffic_daily (
                date TEXT PRIMARY KEY,
                bytes_sent INTEGER DEFAULT 0,
                connection_count INTEGER DEFAULT 0
            );",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Persist the measured session and its daily totals atomically.
    pub fn record_disconnection(
        &self,
        info: &macrdp_core::DisconnectionInfo,
    ) -> Result<(), String> {
        let connected = chrono::DateTime::parse_from_rfc3339(&info.connected_at)
            .map_err(|_| "Invalid connection timestamp".to_string())?;
        let disconnected = chrono::DateTime::parse_from_rfc3339(&info.disconnected_at)
            .map_err(|_| "Invalid disconnection timestamp".to_string())?;
        let day = disconnected
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d")
            .to_string();
        let duration = i64::try_from(info.duration_secs)
            .map_err(|_| "Session duration exceeds database range")?;
        let bytes = i64::try_from(info.bytes_total)
            .map_err(|_| "Session traffic exceeds database range")?;
        let mut conn = self.conn.lock().map_err(|e| e.to_string())?;
        let transaction = conn
            .transaction()
            .map_err(|e| format!("Begin history transaction: {e}"))?;
        transaction.execute(
            "INSERT INTO connections (client_ip, client_name, connected_at, disconnected_at, duration_secs, bytes_total)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![info.client_ip, info.client_name, connected.to_rfc3339(), disconnected.to_rfc3339(), duration, bytes],
        ).map_err(|e| format!("Insert connection history: {e}"))?;
        transaction
            .execute(
                "INSERT INTO traffic_daily (date, bytes_sent, connection_count)
             VALUES (?1, ?2, 1)
             ON CONFLICT(date) DO UPDATE SET
                 bytes_sent = bytes_sent + excluded.bytes_sent,
                 connection_count = connection_count + 1",
                rusqlite::params![day, bytes],
            )
            .map_err(|e| format!("Update daily traffic: {e}"))?;
        transaction
            .commit()
            .map_err(|e| format!("Commit connection history: {e}"))
    }

    /// Retrieve connection history with pagination.
    pub fn get_connection_history(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;

        let mut stmt = conn
            .prepare(
                "SELECT id, client_ip, client_name, connected_at, disconnected_at, duration_secs, bytes_total
                 FROM connections
                 ORDER BY id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let rows = stmt
            .query_map(rusqlite::params![limit, offset], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, i64>(0)?,
                    "client_ip": row.get::<_, String>(1)?,
                    "client_name": row.get::<_, String>(2)?,
                    "connected_at": row.get::<_, String>(3)?,
                    "disconnected_at": row.get::<_, Option<String>>(4)?,
                    "duration_secs": row.get::<_, i64>(5)?,
                    "bytes_total": row.get::<_, i64>(6)?,
                }))
            })
            .map_err(|e| format!("Failed to query connections: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }

        Ok(results)
    }

    /// Retrieve daily traffic statistics for the last N days.
    pub fn get_traffic_stats(&self, days: u32) -> Result<Vec<serde_json::Value>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;

        let mut stmt = conn
            .prepare(
                "SELECT date, bytes_sent, connection_count
                 FROM traffic_daily
                 WHERE date >= date('now', ?1)
                 ORDER BY date DESC",
            )
            .map_err(|e| format!("Failed to prepare query: {}", e))?;

        let days_param = format!("-{} days", days);
        let rows = stmt
            .query_map(rusqlite::params![days_param], |row| {
                Ok(serde_json::json!({
                    "date": row.get::<_, String>(0)?,
                    "bytes_sent": row.get::<_, i64>(1)?,
                    "connection_count": row.get::<_, i64>(2)?,
                }))
            })
            .map_err(|e| format!("Failed to query traffic stats: {}", e))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| format!("Failed to read row: {}", e))?);
        }

        Ok(results)
    }

    /// Retrieve recent log entries (placeholder — logs are not yet stored in DB).
    pub fn get_logs(&self, _limit: usize) -> Result<Vec<serde_json::Value>, String> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> macrdp_core::DisconnectionInfo {
        macrdp_core::DisconnectionInfo {
            client_ip: "192.0.2.1:1234".into(),
            client_name: "test-client".into(),
            connected_at: "2026-01-01T23:59:40Z".into(),
            disconnected_at: "2026-01-02T00:00:10Z".into(),
            duration_secs: 30,
            bytes_total: 12345,
        }
    }

    #[test]
    fn history_preserves_session_times_and_accounts_on_event_date() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::new(&directory.path().join("test.db")).unwrap();
        database.record_disconnection(&session()).unwrap();
        let history = database.get_connection_history(10, 0).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["duration_secs"], 30);
        assert_eq!(history[0]["bytes_total"], 12345);
        assert!(history[0]["connected_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-01-01T23:59:40"));
        let conn = database.conn.lock().unwrap();
        let totals: (String, i64, i64) = conn
            .query_row(
                "SELECT date, bytes_sent, connection_count FROM traffic_daily",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(totals, ("2026-01-02".into(), 12345, 1));
    }

    #[test]
    fn failed_daily_update_rolls_back_the_history_insert() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::new(&directory.path().join("test.db")).unwrap();
        database.conn.lock().unwrap().execute_batch(
            "CREATE TRIGGER reject_totals BEFORE INSERT ON traffic_daily BEGIN SELECT RAISE(ABORT, 'test failure'); END;"
        ).unwrap();
        assert!(database.record_disconnection(&session()).is_err());
        assert!(database.get_connection_history(10, 0).unwrap().is_empty());
    }

    #[test]
    fn unrepresentable_traffic_does_not_wrap_or_create_a_record() {
        let directory = tempfile::tempdir().unwrap();
        let database = Database::new(&directory.path().join("test.db")).unwrap();
        let mut info = session();
        info.bytes_total = u64::MAX;
        assert!(database.record_disconnection(&info).is_err());
        assert!(database.get_connection_history(10, 0).unwrap().is_empty());
    }
}
