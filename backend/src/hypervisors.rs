//! Registre des hyperviseurs : le local (variable `LIBVIRT_URI`) et des hôtes distants
//! (`qemu+ssh://` ou `qemu+tls://`) enregistrés par un administrateur.
//!
//! L'hyperviseur ciblé par une requête est choisi par l'en-tête `X-Hypervisor` (ou `?hv=` pour les
//! WebSocket) ; sans indication, c'est `local`. Les URI libvirt acceptent des paramètres capables
//! d'exécuter des commandes (`command=`, `proxy=`, `netcat=`…) : seule une liste blanche stricte
//! de paramètres inoffensifs est admise.
use crate::{api_error, util::write_private, ApiErr, ApiResult, Message, Shared};
use axum::{
    extract::{FromRequestParts, Path, State},
    http::{request::Parts, StatusCode},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path as FsPath,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use utoipa::ToSchema;
use virt::connect::Connect;

pub const LOCAL_ID: &str = "local";
const BREAKER_SECS: u64 = 15;

#[derive(Clone, Serialize, Deserialize, Debug)]
struct HvEntry {
    id: String,
    name: String,
    uri: String,
}

/// Disjoncteur : après un échec de connexion distante, les requêtes suivantes échouent
/// immédiatement pendant quelques secondes au lieu d'empiler des connexions SSH bloquées.
#[derive(Default)]
pub struct Health {
    down_until: Mutex<Option<Instant>>,
}
impl Health {
    fn fail(&self) {
        *self.down_until.lock().unwrap() = Some(Instant::now() + Duration::from_secs(BREAKER_SECS));
    }
    fn ok(&self) {
        *self.down_until.lock().unwrap() = None;
    }
    fn wait(&self) -> Option<u64> {
        let guard = self.down_until.lock().unwrap();
        guard
            .filter(|t| *t > Instant::now())
            .map(|t| (t - Instant::now()).as_secs() + 1)
    }
}

struct Runtime {
    entry: HvEntry,
    health: Arc<Health>,
}

pub struct HvStore {
    path: std::path::PathBuf,
    runtimes: Mutex<Vec<Runtime>>,
}

#[derive(Serialize, ToSchema)]
pub struct HvView {
    pub id: String,
    pub name: String,
    /// URI libvirt (visible uniquement des administrateurs).
    pub uri: Option<String>,
    pub host: Option<String>,
    /// `local`, `ssh` ou `tls`.
    pub kind: String,
    pub builtin: bool,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewHypervisor {
    /// Identifiant court `[a-z0-9-]` (généré depuis le nom s'il est absent).
    pub id: Option<String>,
    pub name: String,
    /// `qemu+ssh://[user@]hôte[:port]/system[?keyfile=…]` ou `qemu+tls://hôte/system`.
    pub uri: String,
}

#[derive(Serialize, ToSchema)]
pub struct HypervisorInfo {
    pub hostname: String,
    pub hypervisor_type: String,
    pub uri: String,
}

#[derive(Serialize, ToSchema)]
pub struct TestResult {
    pub ok: bool,
    pub hostname: Option<String>,
    pub hypervisor_type: Option<String>,
    pub domains: Option<usize>,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Analyse et validation des URI
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUri {
    pub scheme: String,
    pub user: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub path: String,
    pub params: Vec<(String, String)>,
}

pub fn parse_libvirt_uri(uri: &str) -> Result<ParsedUri, String> {
    let (scheme, rest) = uri
        .split_once("://")
        .ok_or("URI invalide : « schéma:// » attendu")?;
    let (before_query, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let (authority, path) = match before_query.find('/') {
        Some(i) => (&before_query[..i], &before_query[i..]),
        None => (before_query, ""),
    };
    let (user, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u.to_string()), h),
        None => (None, authority),
    };
    let (host, port) = if hostport.starts_with('[') {
        let end = hostport.find(']').ok_or("Adresse IPv6 mal formée")?;
        let host = hostport[..=end].to_string();
        let port = match &hostport[end + 1..] {
            "" => None,
            p => Some(
                p.strip_prefix(':')
                    .and_then(|p| p.parse::<u16>().ok())
                    .ok_or("Port invalide")?,
            ),
        };
        (host, port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                Some(p.parse::<u16>().map_err(|_| "Port invalide")?),
            ),
            None => (hostport.to_string(), None),
        }
    };
    let params = query
        .map(|q| {
            q.split('&')
                .filter(|s| !s.is_empty())
                .map(|kv| match kv.split_once('=') {
                    Some((k, v)) => (k.to_string(), v.to_string()),
                    None => (kv.to_string(), String::new()),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ParsedUri {
        scheme: scheme.to_string(),
        user,
        host: if host.is_empty() { None } else { Some(host) },
        port,
        path: path.to_string(),
        params,
    })
}

fn valid_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return !inner.is_empty() && inner.chars().all(|c| c.is_ascii_hexdigit() || c == ':');
    }
    !host.is_empty()
        && host.len() <= 253
        && host.as_bytes()[0].is_ascii_alphanumeric()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn valid_ssh_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 32
        && user.as_bytes()[0].is_ascii_alphanumeric()
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn safe_path_value(v: &str) -> bool {
    v.starts_with('/')
        && !v.contains("..")
        && v.len() <= 255
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~'))
}

/// Valide une URI saisie par un administrateur et retourne sa forme canonique.
pub fn validate_remote_uri(uri: &str) -> Result<String, String> {
    if uri.len() > 400 || uri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("URI invalide (caractères interdits)".into());
    }
    let p = parse_libvirt_uri(uri)?;
    let ssh = match p.scheme.as_str() {
        "qemu+ssh" => true,
        "qemu+tls" => false,
        _ => return Err("Schéma accepté : qemu+ssh:// ou qemu+tls:// uniquement".into()),
    };
    let host = p.host.as_deref().ok_or("Nom d'hôte requis")?;
    if !valid_host(host) {
        return Err("Nom d'hôte invalide".into());
    }
    if let Some(u) = &p.user {
        if !ssh || !valid_ssh_user(u) {
            return Err("Utilisateur SSH invalide (qemu+ssh uniquement)".into());
        }
    }
    if p.path != "/system" {
        return Err("Le chemin doit être /system".into());
    }
    let mut params: Vec<(String, String)> = Vec::new();
    for (k, v) in &p.params {
        let ok = match k.as_str() {
            "keyfile" if ssh => safe_path_value(v),
            "pkipath" if !ssh => safe_path_value(v),
            "no_tty" | "no_verify" => v == "0" || v == "1",
            "known_hosts_verify" if ssh => matches!(v.as_str(), "normal" | "auto" | "ignore"),
            _ => {
                return Err(format!(
                    "Paramètre d'URI non autorisé : « {k} » (autorisés : keyfile, pkipath, no_tty, no_verify, known_hosts_verify)"
                ))
            }
        };
        if !ok {
            return Err(format!("Valeur invalide pour « {k} »"));
        }
        if params.iter().any(|(pk, _)| pk == k) {
            return Err(format!("Paramètre dupliqué : « {k} »"));
        }
        params.push((k.clone(), v.clone()));
    }
    if ssh && !params.iter().any(|(k, _)| k == "no_tty") {
        // Évite qu'ssh attende un mot de passe sur un terminal inexistant.
        params.push(("no_tty".into(), "1".into()));
    }
    let mut out = format!("{}://", p.scheme);
    if let Some(u) = &p.user {
        out.push_str(u);
        out.push('@');
    }
    out.push_str(host);
    if let Some(port) = p.port {
        out.push_str(&format!(":{port}"));
    }
    out.push_str("/system");
    if !params.is_empty() {
        out.push('?');
        out.push_str(
            &params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    Ok(out)
}

fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn slug(name: &str) -> String {
    let mut s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s = s.trim_matches('-').to_string();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    s.chars().take(32).collect()
}

fn kind_of(uri: &str) -> &'static str {
    if uri.starts_with("qemu+ssh://") {
        "ssh"
    } else if uri.starts_with("qemu+tls://") {
        "tls"
    } else {
        "local"
    }
}

// ---------------------------------------------------------------------------------------------
// Magasin
// ---------------------------------------------------------------------------------------------

impl HvStore {
    pub fn load(data_dir: &FsPath, local_uri: &str) -> Result<Self, String> {
        let path = data_dir.join("hypervisors.json");
        let stored: Vec<HvEntry> = match std::fs::read_to_string(&path) {
            Ok(t) => {
                serde_json::from_str(&t).map_err(|e| format!("hypervisors.json illisible : {e}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.to_string()),
        };
        let mut runtimes = vec![Runtime {
            entry: HvEntry {
                id: LOCAL_ID.into(),
                name: "Hyperviseur local".into(),
                uri: local_uri.into(),
            },
            health: Arc::new(Health::default()),
        }];
        for entry in stored {
            // Une entrée modifiée à la main est revalidée : jamais d'URI non conforme.
            match validate_remote_uri(&entry.uri) {
                Ok(uri) if valid_id(&entry.id) && entry.id != LOCAL_ID => runtimes.push(Runtime {
                    entry: HvEntry { uri, ..entry },
                    health: Arc::new(Health::default()),
                }),
                _ => eprintln!("Hyperviseur « {} » ignoré (entrée invalide)", entry.id),
            }
        }
        Ok(Self {
            path,
            runtimes: Mutex::new(runtimes),
        })
    }

    fn persist(&self, rt: &[Runtime]) -> Result<(), ApiErr> {
        let entries: Vec<&HvEntry> = rt
            .iter()
            .filter(|r| r.entry.id != LOCAL_ID)
            .map(|r| &r.entry)
            .collect();
        let json = serde_json::to_vec_pretty(&entries)
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        write_private(&self.path, &json)
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
    }

    pub fn app_state(&self, id: &str) -> Result<AppState, ApiErr> {
        let rt = self.runtimes.lock().unwrap();
        let r = rt.iter().find(|r| r.entry.id == id).ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                format!("Hyperviseur « {id} » inconnu"),
            )
        })?;
        Ok(AppState {
            libvirt_uri: r.entry.uri.clone(),
            hv_id: r.entry.id.clone(),
            health: (r.entry.id != LOCAL_ID).then(|| r.health.clone()),
        })
    }
}

/// Contexte d'une requête : l'hyperviseur ciblé. Utilisé comme extracteur par tous les handlers.
#[derive(Clone)]
pub struct AppState {
    pub libvirt_uri: String,
    pub hv_id: String,
    health: Option<Arc<Health>>,
}

impl AppState {
    fn track(&self, result: Result<Connect, virt::error::Error>) -> Result<Connect, String> {
        match result {
            Ok(c) => {
                if let Some(h) = &self.health {
                    h.ok();
                }
                Ok(c)
            }
            Err(e) => {
                if let Some(h) = &self.health {
                    h.fail();
                }
                Err(format!(
                    "Connexion libvirt impossible (hyperviseur « {} ») : {e}",
                    self.hv_id
                ))
            }
        }
    }
    /// Connexion lecture/écriture (opérations modifiantes).
    pub fn open_rw(&self) -> Result<Connect, String> {
        self.track(Connect::open(Some(&self.libvirt_uri)))
    }
    /// Connexion en lecture seule.
    pub fn open_ro(&self) -> Result<Connect, String> {
        self.track(Connect::open_read_only(Some(&self.libvirt_uri)))
    }
    pub fn is_local(&self) -> bool {
        self.hv_id == LOCAL_ID
    }
}

impl FromRequestParts<Shared> for AppState {
    type Rejection = ApiErr;
    async fn from_request_parts(parts: &mut Parts, shared: &Shared) -> Result<Self, ApiErr> {
        let from_header = parts
            .headers
            .get("x-hypervisor")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let from_query = parts.uri.query().and_then(|q| {
            q.split('&')
                .find_map(|kv| kv.strip_prefix("hv=").map(str::to_string))
        });
        let id = from_header
            .or(from_query)
            .unwrap_or_else(|| LOCAL_ID.into());
        if !valid_id(&id) {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "Identifiant d'hyperviseur invalide",
            ));
        }
        let state = shared.hvs.app_state(&id)?;
        if let Some(wait) = state.health.as_ref().and_then(|h| h.wait()) {
            return Err(api_error(
                StatusCode::BAD_GATEWAY,
                format!("Hyperviseur « {id} » injoignable (nouvel essai dans {wait} s)"),
            ));
        }
        Ok(state)
    }
}

// ---------------------------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/hypervisors", responses((status = 200, body = [HvView])), tag = "Hyperviseurs")]
pub async fn list_hypervisors(
    State(s): State<Shared>,
    Extension(user): Extension<crate::auth::CurrentUser>,
) -> Json<Vec<HvView>> {
    let admin = user.role == crate::auth::Role::Admin;
    let rt = s.hvs.runtimes.lock().unwrap();
    Json(
        rt.iter()
            .map(|r| HvView {
                id: r.entry.id.clone(),
                name: r.entry.name.clone(),
                uri: admin.then(|| r.entry.uri.clone()),
                host: parse_libvirt_uri(&r.entry.uri).ok().and_then(|p| p.host),
                kind: kind_of(&r.entry.uri).into(),
                builtin: r.entry.id == LOCAL_ID,
            })
            .collect(),
    )
}

#[utoipa::path(post, path = "/api/v1/hypervisors", request_body = NewHypervisor,
    responses((status = 201, body = HvView), (status = 400, body = crate::ApiError), (status = 409, body = crate::ApiError)),
    tag = "Hyperviseurs")]
pub async fn add_hypervisor(
    State(s): State<Shared>,
    Json(req): Json<NewHypervisor>,
) -> Result<(StatusCode, Json<HvView>), ApiErr> {
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Nom invalide (1 à 64 caractères)",
        ));
    }
    let uri =
        validate_remote_uri(req.uri.trim()).map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
    let id = req
        .id
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .unwrap_or_else(|| slug(&name));
    if !valid_id(&id) || id == LOCAL_ID {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Identifiant : 1 à 32 caractères [a-z0-9-] (« local » est réservé)",
        ));
    }
    let mut rt = s.hvs.runtimes.lock().unwrap();
    if rt.iter().any(|r| r.entry.id == id) {
        return Err(api_error(
            StatusCode::CONFLICT,
            format!("L'identifiant « {id} » existe déjà"),
        ));
    }
    let entry = HvEntry { id, name, uri };
    let view = HvView {
        id: entry.id.clone(),
        name: entry.name.clone(),
        uri: Some(entry.uri.clone()),
        host: parse_libvirt_uri(&entry.uri).ok().and_then(|p| p.host),
        kind: kind_of(&entry.uri).into(),
        builtin: false,
    };
    rt.push(Runtime {
        entry,
        health: Arc::new(Health::default()),
    });
    s.hvs.persist(&rt)?;
    Ok((StatusCode::CREATED, Json(view)))
}

#[utoipa::path(delete, path = "/api/v1/hypervisors/{id}", params(("id" = String, Path)),
    responses((status = 200, body = Message), (status = 404, body = crate::ApiError), (status = 409, body = crate::ApiError)),
    tag = "Hyperviseurs")]
pub async fn delete_hypervisor(
    State(s): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<Message> {
    if id == LOCAL_ID {
        return Err(api_error(
            StatusCode::CONFLICT,
            "L'hyperviseur local ne peut pas être retiré",
        ));
    }
    let mut rt = s.hvs.runtimes.lock().unwrap();
    let before = rt.len();
    rt.retain(|r| r.entry.id != id);
    if rt.len() == before {
        return Err(api_error(StatusCode::NOT_FOUND, "Hyperviseur inconnu"));
    }
    s.hvs.persist(&rt)?;
    Ok(Json(Message {
        message: format!("Hyperviseur « {id} » retiré de la liste (aucune VM n'est touchée)."),
    }))
}

#[utoipa::path(post, path = "/api/v1/hypervisors/{id}/test", params(("id" = String, Path)), request_body = crate::ActionRequest,
    responses((status = 200, body = TestResult), (status = 404, body = crate::ApiError)), tag = "Hyperviseurs")]
pub async fn test_hypervisor(
    State(s): State<Shared>,
    Path(id): Path<String>,
    _body: Json<crate::ActionRequest>,
) -> ApiResult<TestResult> {
    let app = s.hvs.app_state(&id)?;
    let job = tokio::task::spawn_blocking(move || -> Result<TestResult, String> {
        let conn = app.open_ro()?;
        Ok(TestResult {
            ok: true,
            hostname: conn.get_hostname().ok(),
            hypervisor_type: conn.get_type().ok(),
            domains: conn.list_all_domains(0).ok().map(|d| d.len()),
            error: None,
        })
    });
    let result = match tokio::time::timeout(Duration::from_secs(25), job).await {
        Ok(Ok(Ok(r))) => r,
        Ok(Ok(Err(e))) => failed(e),
        Ok(Err(e)) => failed(e.to_string()),
        Err(_) => failed(
            "Délai dépassé (25 s) : hôte injoignable ou authentification SSH interactive".into(),
        ),
    };
    Ok(Json(result))
}

fn failed(error: String) -> TestResult {
    TestResult {
        ok: false,
        hostname: None,
        hypervisor_type: None,
        domains: None,
        error: Some(error),
    }
}

#[utoipa::path(get, path = "/api/v1/hypervisors/{id}/info", params(("id" = String, Path)),
    responses((status = 200, body = HypervisorInfo), (status = 404, body = crate::ApiError), (status = 502, body = crate::ApiError)),
    tag = "Hyperviseurs")]
pub async fn hypervisor_info(
    State(s): State<Shared>,
    Path(id): Path<String>,
) -> ApiResult<HypervisorInfo> {
    let app = s.hvs.app_state(&id)?;
    if let Some(wait) = app.health.as_ref().and_then(|h| h.wait()) {
        return Err(api_error(
            StatusCode::BAD_GATEWAY,
            format!("Hyperviseur « {id} » injoignable (nouvel essai dans {wait} s)"),
        ));
    }
    let info = tokio::task::spawn_blocking(move || -> Result<HypervisorInfo, String> {
        let conn = app.open_ro()?;
        Ok(HypervisorInfo {
            hostname: conn.get_hostname().map_err(|e| e.to_string())?,
            hypervisor_type: conn.get_type().map_err(|e| e.to_string())?,
            uri: conn.get_uri().map_err(|e| e.to_string())?,
        })
    })
    .await
    .map_err(|e| crate::libvirt_error(e.to_string()))?
    .map_err(crate::libvirt_error)?;
    Ok(Json(info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uris() {
        let p = parse_libvirt_uri(
            "qemu+ssh://root@lab.example.org:2222/system?keyfile=/home/a/.ssh/id",
        )
        .unwrap();
        assert_eq!(p.user.as_deref(), Some("root"));
        assert_eq!(p.host.as_deref(), Some("lab.example.org"));
        assert_eq!(p.port, Some(2222));
        assert_eq!(p.path, "/system");
        assert_eq!(p.params, vec![("keyfile".into(), "/home/a/.ssh/id".into())]);
        let l = parse_libvirt_uri("qemu:///system").unwrap();
        assert_eq!(l.host, None);
        assert_eq!(l.path, "/system");
        let v6 = parse_libvirt_uri("qemu+tls://[fe80::1]:16514/system").unwrap();
        assert_eq!(v6.host.as_deref(), Some("[fe80::1]"));
        assert_eq!(v6.port, Some(16514));
    }

    #[test]
    fn accepts_safe_remote_uris_and_canonicalizes() {
        assert_eq!(
            validate_remote_uri("qemu+ssh://bob@10.0.0.5/system").unwrap(),
            "qemu+ssh://bob@10.0.0.5/system?no_tty=1"
        );
        assert_eq!(
            validate_remote_uri("qemu+tls://hv1.lab/system").unwrap(),
            "qemu+tls://hv1.lab/system"
        );
        assert!(validate_remote_uri(
            "qemu+ssh://h/system?keyfile=/home/u/.ssh/id_ed25519&no_tty=0"
        )
        .is_ok());
    }

    #[test]
    fn rejects_dangerous_uris() {
        for bad in [
            "qemu:///system",
            "qemu+tcp://h/system",
            "qemu+ssh://h/session",
            "qemu+ssh://h/system?command=/bin/sh",
            "qemu+ssh://h/system?proxy=x",
            "qemu+ssh://h/system?netcat=nc",
            "qemu+ssh://h/system?keyfile=/home/../etc/shadow",
            "qemu+ssh://h/system?keyfile=relative",
            "qemu+ssh://-oProxyCommand=x/system",
            "qemu+ssh://u ser@h/system",
            "qemu+ssh://h;rm/system",
            "qemu+ssh:///system",
            "qemu+tls://u@h/system",
            "qemu+tls://h/system?keyfile=/a",
            "qemu+ssh://h/system?no_tty=1&no_tty=0",
            "test:///default",
        ] {
            assert!(
                validate_remote_uri(bad).is_err(),
                "devrait être refusée : {bad}"
            );
        }
    }

    #[test]
    fn ids_and_slugs() {
        assert!(valid_id("lab-1"));
        assert!(!valid_id("Lab"));
        assert!(!valid_id("-x"));
        assert!(!valid_id(""));
        assert_eq!(slug("Mon Labo n°2 !"), "mon-labo-n-2");
    }

    #[test]
    fn breaker_blocks_then_recovers() {
        let h = Health::default();
        assert!(h.wait().is_none());
        h.fail();
        assert!(h.wait().is_some());
        h.ok();
        assert!(h.wait().is_none());
    }
}
