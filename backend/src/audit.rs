//! Journal d'audit JSON Lines : qui a fait quoi, quand, depuis où, avec quel résultat.
//! Les mots de passe et corps de requêtes ne sont jamais enregistrés.
use crate::util::{now_unix, rfc3339};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::Mutex,
};
use utoipa::ToSchema;

#[derive(Serialize, Deserialize, ToSchema, Clone, Debug)]
pub struct AuditEntry {
    pub ts: String,
    pub user: String,
    pub ip: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub hypervisor: Option<String>,
}

pub struct Audit {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl Audit {
    pub fn open(dir: &std::path::Path) -> Self {
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("audit.log");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| eprintln!("Audit désactivé ({}) : {e}", path.display()))
            .ok();
        Self {
            path,
            file: Mutex::new(file),
        }
    }

    pub fn record(
        &self,
        user: &str,
        ip: &str,
        method: &str,
        path: &str,
        status: u16,
        hypervisor: Option<String>,
    ) {
        let entry = AuditEntry {
            ts: rfc3339(now_unix()),
            user: user.chars().take(64).collect(),
            ip: ip.into(),
            method: method.into(),
            path: path.chars().take(300).collect(),
            status,
            hypervisor,
        };
        if let (Ok(line), Ok(mut guard)) = (serde_json::to_string(&entry), self.file.lock()) {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    /// Dernières entrées, la plus récente en premier.
    pub fn tail(&self, limit: usize) -> Vec<AuditEntry> {
        let Ok(file) = File::open(&self.path) else {
            return vec![];
        };
        let mut ring: VecDeque<AuditEntry> = VecDeque::with_capacity(limit + 1);
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(e) = serde_json::from_str::<AuditEntry>(&line) {
                ring.push_back(e);
                if ring.len() > limit {
                    ring.pop_front();
                }
            }
        }
        ring.into_iter().rev().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_and_tails_latest_first() {
        let dir = std::env::temp_dir().join(format!("vm-audit-{}", uuid::Uuid::new_v4()));
        let audit = Audit::open(&dir);
        for i in 0..5 {
            audit.record("alice", "127.0.0.1", "POST", &format!("/x/{i}"), 200, None);
        }
        let t = audit.tail(3);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].path, "/x/4");
        assert_eq!(t[2].path, "/x/2");
        let _ = std::fs::remove_dir_all(dir);
    }
}
