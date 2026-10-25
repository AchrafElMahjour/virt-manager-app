//! Configuration lue depuis l'environnement. Aucune valeur secrète n'est codée en dur.
use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    /// URI libvirt de l'hyperviseur local (« local » dans l'interface).
    pub local_uri: String,
    pub bind: String,
    /// Répertoire des données de l'application (utilisateurs, hyperviseurs, audit).
    pub data_dir: PathBuf,
    /// Répertoire du front-end compilé (frontend/dist). Absent : API seule.
    pub static_dir: Option<PathBuf>,
    /// Ajoute l'attribut Secure au cookie de session (à activer derrière HTTPS).
    pub cookie_secure: bool,
    /// Origines navigateur autorisées en plus de l'origine identique (mode dev Vite).
    pub allowed_origins: Vec<String>,
    pub session_ttl_secs: u64,
    pub max_upload_bytes: u64,
    /// Racines autorisées pour la création de pools « dir ».
    pub pool_roots: Vec<String>,
}

fn list(name: &str, default: &str) -> Vec<String> {
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn flag(name: &str) -> bool {
    matches!(
        env::var(name).as_deref(),
        Ok("1" | "true" | "yes" | "on" | "TRUE" | "True")
    )
}

impl Config {
    pub fn from_env() -> Self {
        let data_dir = env::var("VIRTMGR_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let base = env::var("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .or_else(|_| env::var("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                    .unwrap_or_else(|_| PathBuf::from("."));
                base.join("virt-manager-app")
            });
        let static_dir = env::var("VIRTMGR_STATIC_DIR")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                ["../frontend/dist", "frontend/dist"]
                    .iter()
                    .map(PathBuf::from)
                    .find(|p| p.join("index.html").is_file())
            })
            .filter(|p| p.join("index.html").is_file());
        Self {
            local_uri: env::var("LIBVIRT_URI").unwrap_or_else(|_| "qemu:///system".into()),
            bind: env::var("API_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into()),
            data_dir,
            static_dir,
            cookie_secure: flag("VIRTMGR_COOKIE_SECURE"),
            allowed_origins: list(
                "VIRTMGR_ALLOWED_ORIGINS",
                "http://127.0.0.1:5173,http://localhost:5173",
            ),
            session_ttl_secs: env::var("VIRTMGR_SESSION_HOURS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|h| (1..=720).contains(h))
                .unwrap_or(12)
                * 3600,
            max_upload_bytes: env::var("VIRTMGR_MAX_UPLOAD_GIB")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|g| (1..=256).contains(g))
                .unwrap_or(16)
                * 1024
                * 1024
                * 1024,
            pool_roots: list(
                "VIRTMGR_POOL_ROOTS",
                "/var/lib/libvirt,/srv,/mnt,/media,/opt,/data,/home",
            ),
        }
    }
}
