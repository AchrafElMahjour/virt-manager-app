//! Authentification, rôles, sessions et garde des routes.
//!
//! * Mots de passe : Argon2id (paramètres par défaut de la crate `argon2`), jamais en clair.
//! * Sessions : jeton aléatoire de 256 bits, conservé **en mémoire** côté serveur, transmis dans un
//!   cookie `HttpOnly; SameSite=Strict` (inaccessible au JavaScript, non envoyé par un site tiers).
//! * Rôles : `viewer` < `operator` < `admin`, vérifiés pour **chaque** requête par [`guard`].
//! * Anti-force-brute : verrouillage progressif par couple adresse IP + identifiant.
//! * Anti-CSRF : contrôle de l'en-tête `Origin` sur les requêtes modifiantes et les WebSocket.
use crate::{
    api_error,
    util::{now_unix, write_private},
    ApiErr, ApiResult, Shared,
};
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{ConnectInfo, Extension, Path, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use utoipa::ToSchema;

pub const SESSION_COOKIE: &str = "vm_session";
const MAX_FAILS_BEFORE_LOCK: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Lecture seule.
    Viewer,
    /// Lecture + démarrer/arrêter/suspendre/reprendre + console.
    Operator,
    /// Tous les droits (création, suppression, stockage, utilisateurs, hyperviseurs, audit).
    Admin,
}

#[derive(Clone, Serialize, Deserialize)]
struct UserRecord {
    username: String,
    password_hash: String,
    role: Role,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    created_at: u64,
}

#[derive(Serialize, ToSchema, Clone)]
pub struct UserView {
    pub username: String,
    pub role: Role,
    pub disabled: bool,
    pub created_at: u64,
}

impl From<&UserRecord> for UserView {
    fn from(u: &UserRecord) -> Self {
        Self {
            username: u.username.clone(),
            role: u.role,
            disabled: u.disabled,
            created_at: u.created_at,
        }
    }
}

/// Utilisateur authentifié, injecté dans les extensions de la requête par [`guard`].
#[derive(Clone, Serialize, ToSchema, Debug)]
pub struct CurrentUser {
    pub username: String,
    pub role: Role,
}

struct Session {
    username: String,
    last_seen: Instant,
}

#[derive(Default)]
struct Attempt {
    fails: u32,
    locked_until: Option<Instant>,
}

pub struct AuthStore {
    path: PathBuf,
    initial_pw_file: PathBuf,
    users: Mutex<Vec<UserRecord>>,
    sessions: Mutex<HashMap<String, Session>>,
    attempts: Mutex<HashMap<String, Attempt>>,
    ttl: Duration,
    dummy_hash: String,
}

// ---------------------------------------------------------------------------------------------
// Mots de passe et jetons
// ---------------------------------------------------------------------------------------------

fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_password() -> String {
    // Alphabet sans caractères ambigus (0/O, 1/l/I) pour une saisie facile.
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0_u8; 20];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

fn check_password_policy(password: &str) -> Result<(), ApiErr> {
    let n = password.chars().count();
    if !(10..=256).contains(&n) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Le mot de passe doit contenir entre 10 et 256 caractères",
        ));
    }
    Ok(())
}

fn valid_username(name: &str) -> bool {
    let b = name.as_bytes();
    (3..=32).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

// ---------------------------------------------------------------------------------------------
// Magasin d'utilisateurs et de sessions
// ---------------------------------------------------------------------------------------------

impl AuthStore {
    /// Charge `users.json` ; au premier lancement crée le compte `admin`.
    /// Retourne le mot de passe généré s'il y en a un (à afficher une seule fois).
    pub fn load(data_dir: &FsPath, ttl_secs: u64) -> Result<(Self, Option<String>), String> {
        let path = data_dir.join("users.json");
        let initial_pw_file = data_dir.join("admin-initial-password.txt");
        let mut users: Vec<UserRecord> = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| format!("users.json illisible : {e}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("Lecture de {} impossible : {e}", path.display())),
        };
        let dummy_hash = hash_password(&random_token())?;
        let mut generated = None;
        if users.is_empty() {
            let (password, was_generated) = match std::env::var("VIRTMGR_ADMIN_PASSWORD") {
                Ok(p) if p.chars().count() >= 10 => (p, false),
                Ok(_) => {
                    return Err(
                        "VIRTMGR_ADMIN_PASSWORD doit contenir au moins 10 caractères".into(),
                    )
                }
                Err(_) => (random_password(), true),
            };
            users.push(UserRecord {
                username: "admin".into(),
                password_hash: hash_password(&password)?,
                role: Role::Admin,
                disabled: false,
                created_at: now_unix(),
            });
            let json = serde_json::to_vec_pretty(&users).map_err(|e| e.to_string())?;
            write_private(&path, &json).map_err(|e| format!("Écriture users.json : {e}"))?;
            if was_generated {
                let note = format!(
                    "Compte initial Virt Manager\nidentifiant : admin\nmot de passe : {password}\n\nCe fichier est supprimé automatiquement dès que le mot de passe est changé dans l'interface.\n"
                );
                let _ = write_private(&initial_pw_file, note.as_bytes());
                generated = Some(password);
            }
        }
        Ok((
            Self {
                path,
                initial_pw_file,
                users: Mutex::new(users),
                sessions: Mutex::new(HashMap::new()),
                attempts: Mutex::new(HashMap::new()),
                ttl: Duration::from_secs(ttl_secs),
                dummy_hash,
            },
            generated,
        ))
    }

    fn persist(&self, users: &[UserRecord]) -> Result<(), ApiErr> {
        let json = serde_json::to_vec_pretty(users)
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        write_private(&self.path, &json).map_err(|e| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Écriture des utilisateurs impossible : {e}"),
            )
        })
    }

    fn check_credentials(&self, username: &str, password: &str) -> Option<CurrentUser> {
        let record = self
            .users
            .lock()
            .ok()?
            .iter()
            .find(|u| u.username == username)
            .cloned();
        match record {
            Some(u) if !u.disabled && verify_password(&u.password_hash, password) => {
                Some(CurrentUser {
                    username: u.username,
                    role: u.role,
                })
            }
            Some(_) => None,
            None => {
                // Même coût CPU qu'un vrai utilisateur : pas d'énumération par le temps de réponse.
                let _ = verify_password(&self.dummy_hash, password);
                None
            }
        }
    }

    fn create_session(&self, username: &str) -> String {
        let token = random_token();
        if let Ok(mut sessions) = self.sessions.lock() {
            if sessions.len() > 256 {
                let ttl = self.ttl;
                sessions.retain(|_, s| s.last_seen.elapsed() < ttl);
            }
            sessions.insert(
                token.clone(),
                Session {
                    username: username.to_string(),
                    last_seen: Instant::now(),
                },
            );
        }
        token
    }

    /// Résout un jeton en utilisateur courant (rôle relu à chaque fois) et prolonge la session.
    pub fn user_for_token(&self, token: &str) -> Option<CurrentUser> {
        let username = {
            let mut sessions = self.sessions.lock().ok()?;
            let session = sessions.get_mut(token)?;
            if session.last_seen.elapsed() >= self.ttl {
                sessions.remove(token);
                return None;
            }
            session.last_seen = Instant::now();
            session.username.clone()
        };
        let users = self.users.lock().ok()?;
        let user = users.iter().find(|u| u.username == username)?;
        if user.disabled {
            return None;
        }
        Some(CurrentUser {
            username: user.username.clone(),
            role: user.role,
        })
    }

    fn destroy_session(&self, token: &str) {
        if let Ok(mut s) = self.sessions.lock() {
            s.remove(token);
        }
    }

    fn purge_user_sessions(&self, username: &str, keep: Option<&str>) {
        if let Ok(mut s) = self.sessions.lock() {
            s.retain(|tok, sess| sess.username != username || Some(tok.as_str()) == keep);
        }
    }

    fn locked_for(&self, key: &str) -> Option<u64> {
        let mut map = self.attempts.lock().ok()?;
        let attempt = map.get_mut(key)?;
        match attempt.locked_until {
            Some(until) if until > Instant::now() => Some((until - Instant::now()).as_secs() + 1),
            Some(_) => {
                attempt.locked_until = None;
                None
            }
            None => None,
        }
    }

    fn register_failure(&self, key: &str) {
        if let Ok(mut map) = self.attempts.lock() {
            if map.len() > 2000 {
                map.retain(|_, a| a.locked_until.is_some_and(|u| u > Instant::now()));
            }
            let a = map.entry(key.to_string()).or_default();
            a.fails += 1;
            if a.fails >= MAX_FAILS_BEFORE_LOCK {
                // 30 s, 60 s, 120 s … plafonné à 15 minutes.
                let exp = (a.fails - MAX_FAILS_BEFORE_LOCK).min(5);
                let secs = (30_u64 << exp).min(900);
                a.locked_until = Some(Instant::now() + Duration::from_secs(secs));
            }
        }
    }

    fn clear_failures(&self, key: &str) {
        if let Ok(mut map) = self.attempts.lock() {
            map.remove(key);
        }
    }

    fn enabled_admins(users: &[UserRecord]) -> usize {
        users
            .iter()
            .filter(|u| u.role == Role::Admin && !u.disabled)
            .count()
    }
}

// ---------------------------------------------------------------------------------------------
// Cookies, origine et table des permissions
// ---------------------------------------------------------------------------------------------

pub fn token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, v)| *k == SESSION_COOKIE && !v.is_empty())
        .map(|(_, v)| v.to_string())
}

fn session_cookie(secure: bool, token: &str, max_age: u64) -> String {
    format!(
        "{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    )
}

/// `Origin` absent → autorisé (clients non navigateur) ; sinon il doit viser le même hôte que
/// la requête ou figurer dans la liste blanche (`VIRTMGR_ALLOWED_ORIGINS`).
pub fn origin_allowed(allowed: &[String], headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    if allowed.iter().any(|a| a == origin) {
        return true;
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let authority = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    matches!((authority, host), (Some(a), Some(h)) if a == h)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Access {
    Public,
    /// Toute session valide.
    Authenticated,
    /// Rôle minimal requis.
    Role(Role),
}

/// Table centrale des permissions. Toute route inconnue est réservée aux admins (sauf GET).
pub fn access_for(method: &Method, path: &str) -> Access {
    let read = method == Method::GET || method == Method::HEAD;
    match path {
        "/api/v1/health" if read => return Access::Public,
        "/api/v1/auth/login" if method == Method::POST => return Access::Public,
        "/api/v1/auth/logout" | "/api/v1/auth/me" | "/api/v1/auth/password" => {
            return Access::Authenticated
        }
        _ => {}
    }
    if path.starts_with("/api/v1/users") || path.starts_with("/api/v1/audit") {
        return Access::Role(Role::Admin);
    }
    if let Some(rest) = path.strip_prefix("/api/v1/vms/") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() == 2 {
            match (parts[1], read) {
                ("vnc", true) => return Access::Role(Role::Operator),
                ("start" | "shutdown" | "suspend" | "resume", false) if method == Method::POST => {
                    return Access::Role(Role::Operator)
                }
                _ => {}
            }
        }
    }
    if read {
        Access::Role(Role::Viewer)
    } else {
        Access::Role(Role::Admin)
    }
}

fn client_ip(req: &Request) -> String {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Middleware appliqué à toutes les routes : origine, session, rôle, audit.
pub async fn guard(State(s): State<Shared>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if !(path.starts_with("/api/") || path.starts_with("/api-docs/")) {
        return next.run(req).await; // fichiers statiques du front-end
    }
    let method = req.method().clone();
    let is_ws = req.headers().contains_key(header::UPGRADE);
    let modifying = !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS);
    if (modifying || is_ws) && !origin_allowed(&s.cfg.allowed_origins, req.headers()) {
        return api_error(StatusCode::FORBIDDEN, "Origine de la requête non autorisée")
            .into_response();
    }
    if is_ws && !req.headers().contains_key(header::ORIGIN) {
        return api_error(
            StatusCode::FORBIDDEN,
            "En-tête Origin requis pour WebSocket",
        )
        .into_response();
    }
    let access = access_for(&method, &path);
    if access == Access::Public {
        return no_store(next.run(req).await);
    }
    let ip = client_ip(&req);
    let token = token_from_headers(req.headers());
    let Some(user) = token.as_deref().and_then(|t| s.auth.user_for_token(t)) else {
        return api_error(StatusCode::UNAUTHORIZED, "Authentification requise").into_response();
    };
    if let Access::Role(min) = access {
        if user.role < min {
            s.audit
                .record(&user.username, &ip, method.as_str(), &path, 403, None);
            return api_error(
                StatusCode::FORBIDDEN,
                "Droits insuffisants pour cette opération",
            )
            .into_response();
        }
    }
    let hv = req
        .headers()
        .get("x-hypervisor")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    req.extensions_mut().insert(user.clone());
    let response = next.run(req).await;
    if modifying || is_ws {
        s.audit.record(
            &user.username,
            &ip,
            method.as_str(),
            &path,
            response.status().as_u16(),
            hv,
        );
    }
    no_store(response)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

// ---------------------------------------------------------------------------------------------
// Routes /auth
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[utoipa::path(post, path = "/api/v1/auth/login", request_body = LoginRequest,
    responses((status = 200, body = CurrentUser), (status = 401, body = crate::ApiError), (status = 429, body = crate::ApiError)),
    tag = "Authentification")]
pub async fn login(
    State(s): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiErr> {
    let username = req.username.trim().to_lowercase();
    let ip = addr.ip().to_string();
    let key = format!("{ip}|{username}");
    if let Some(wait) = s.auth.locked_for(&key) {
        s.audit
            .record(&username, &ip, "POST", "/api/v1/auth/login", 429, None);
        return Err(api_error(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Trop de tentatives échouées. Réessayez dans {wait} s."),
        ));
    }
    let auth = s.auth.clone();
    let (u, p) = (username.clone(), req.password);
    let user = tokio::task::spawn_blocking(move || auth.check_credentials(&u, &p))
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(user) = user else {
        s.auth.register_failure(&key);
        s.audit
            .record(&username, &ip, "POST", "/api/v1/auth/login", 401, None);
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "Identifiant ou mot de passe incorrect",
        ));
    };
    s.auth.clear_failures(&key);
    let token = s.auth.create_session(&user.username);
    s.audit
        .record(&user.username, &ip, "POST", "/api/v1/auth/login", 200, None);
    let cookie = session_cookie(s.cfg.cookie_secure, &token, s.cfg.session_ttl_secs);
    Ok(([(header::SET_COOKIE, cookie)], Json(user)).into_response())
}

#[utoipa::path(post, path = "/api/v1/auth/logout", request_body = crate::ActionRequest,
    responses((status = 200, description = "Session fermée")), tag = "Authentification")]
pub async fn logout(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(_req): Json<crate::ActionRequest>,
) -> Response {
    if let Some(token) = token_from_headers(&headers) {
        s.auth.destroy_session(&token);
    }
    let cookie = session_cookie(s.cfg.cookie_secure, "", 0);
    (
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({"message": "Déconnecté"})),
    )
        .into_response()
}

#[utoipa::path(get, path = "/api/v1/auth/me",
    responses((status = 200, body = CurrentUser), (status = 401, body = crate::ApiError)),
    tag = "Authentification")]
pub async fn me(Extension(user): Extension<CurrentUser>) -> Json<CurrentUser> {
    Json(user)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[utoipa::path(post, path = "/api/v1/auth/password", request_body = ChangePasswordRequest,
    responses((status = 200, body = crate::Message), (status = 400, body = crate::ApiError), (status = 401, body = crate::ApiError)),
    tag = "Authentification")]
pub async fn change_password(
    State(s): State<Shared>,
    Extension(user): Extension<CurrentUser>,
    headers: HeaderMap,
    Json(req): Json<ChangePasswordRequest>,
) -> ApiResult<crate::Message> {
    check_password_policy(&req.new_password)?;
    if req.new_password == req.current_password {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Le nouveau mot de passe doit différer de l'actuel",
        ));
    }
    let auth = s.auth.clone();
    let name = user.username.clone();
    let (cur, new) = (req.current_password, req.new_password);
    let new_hash = tokio::task::spawn_blocking(move || -> Result<String, ApiErr> {
        if auth.check_credentials(&name, &cur).is_none() {
            return Err(api_error(
                StatusCode::UNAUTHORIZED,
                "Mot de passe actuel incorrect",
            ));
        }
        hash_password(&new).map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))
    })
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    {
        let mut users = s.auth.users.lock().unwrap();
        let record = users
            .iter_mut()
            .find(|u| u.username == user.username)
            .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Utilisateur introuvable"))?;
        record.password_hash = new_hash;
        s.auth.persist(&users)?;
    }
    // Les autres sessions de ce compte (autres navigateurs) sont fermées.
    let current = token_from_headers(&headers);
    s.auth
        .purge_user_sessions(&user.username, current.as_deref());
    if user.username == "admin" {
        let _ = std::fs::remove_file(&s.auth.initial_pw_file);
    }
    Ok(Json(crate::Message {
        message: "Mot de passe modifié. Les autres sessions ont été fermées.".into(),
    }))
}

// ---------------------------------------------------------------------------------------------
// Routes /users (admin)
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewUserRequest {
    pub username: String,
    pub password: String,
    pub role: Role,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserRequest {
    pub role: Option<Role>,
    pub disabled: Option<bool>,
    /// Réinitialisation du mot de passe par un administrateur.
    pub password: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/users", responses((status = 200, body = [UserView])), tag = "Utilisateurs")]
pub async fn list_users(State(s): State<Shared>) -> Json<Vec<UserView>> {
    let mut v: Vec<UserView> = s
        .auth
        .users
        .lock()
        .unwrap()
        .iter()
        .map(Into::into)
        .collect();
    v.sort_by(|a, b| a.username.cmp(&b.username));
    Json(v)
}

#[utoipa::path(post, path = "/api/v1/users", request_body = NewUserRequest,
    responses((status = 201, body = UserView), (status = 400, body = crate::ApiError), (status = 409, body = crate::ApiError)),
    tag = "Utilisateurs")]
pub async fn create_user(
    State(s): State<Shared>,
    Json(req): Json<NewUserRequest>,
) -> Result<(StatusCode, Json<UserView>), ApiErr> {
    let username = req.username.trim().to_lowercase();
    if !valid_username(&username) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Identifiant : 3 à 32 caractères [a-z0-9._-], début alphanumérique",
        ));
    }
    check_password_policy(&req.password)?;
    let password = req.password;
    let hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut users = s.auth.users.lock().unwrap();
    if users.iter().any(|u| u.username == username) {
        return Err(api_error(
            StatusCode::CONFLICT,
            format!("L'utilisateur « {username} » existe déjà"),
        ));
    }
    let record = UserRecord {
        username,
        password_hash: hash,
        role: req.role,
        disabled: false,
        created_at: now_unix(),
    };
    let view = UserView::from(&record);
    users.push(record);
    s.auth.persist(&users)?;
    Ok((StatusCode::CREATED, Json(view)))
}

#[utoipa::path(put, path = "/api/v1/users/{username}", params(("username" = String, Path)), request_body = UpdateUserRequest,
    responses((status = 200, body = UserView), (status = 404, body = crate::ApiError), (status = 409, body = crate::ApiError)),
    tag = "Utilisateurs")]
pub async fn update_user(
    State(s): State<Shared>,
    Extension(me): Extension<CurrentUser>,
    Path(username): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> ApiResult<UserView> {
    if username == me.username && (req.role.is_some() || req.disabled.is_some()) {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Vous ne pouvez pas modifier votre propre rôle ou désactiver votre propre compte",
        ));
    }
    let new_hash = match req.password {
        Some(p) => {
            check_password_policy(&p)?;
            Some(
                tokio::task::spawn_blocking(move || hash_password(&p))
                    .await
                    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
                    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, e))?,
            )
        }
        None => None,
    };
    let mut users = s.auth.users.lock().unwrap();
    let mut updated = users.clone();
    let record = updated
        .iter_mut()
        .find(|u| u.username == username)
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Utilisateur introuvable"))?;
    if let Some(role) = req.role {
        record.role = role;
    }
    if let Some(disabled) = req.disabled {
        record.disabled = disabled;
    }
    let reset = new_hash.is_some();
    if let Some(hash) = new_hash {
        record.password_hash = hash;
    }
    let view = UserView::from(&*record);
    if AuthStore::enabled_admins(&updated) == 0 {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Il doit rester au moins un administrateur actif",
        ));
    }
    s.auth.persist(&updated)?;
    *users = updated;
    drop(users);
    if reset || req.role.is_some() || req.disabled == Some(true) {
        s.auth.purge_user_sessions(&username, None);
    }
    Ok(Json(view))
}

#[utoipa::path(delete, path = "/api/v1/users/{username}", params(("username" = String, Path)),
    responses((status = 200, body = crate::Message), (status = 404, body = crate::ApiError), (status = 409, body = crate::ApiError)),
    tag = "Utilisateurs")]
pub async fn delete_user(
    State(s): State<Shared>,
    Extension(me): Extension<CurrentUser>,
    Path(username): Path<String>,
) -> ApiResult<crate::Message> {
    if username == me.username {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Vous ne pouvez pas supprimer votre propre compte",
        ));
    }
    let mut users = s.auth.users.lock().unwrap();
    let mut updated = users.clone();
    let before = updated.len();
    updated.retain(|u| u.username != username);
    if updated.len() == before {
        return Err(api_error(StatusCode::NOT_FOUND, "Utilisateur introuvable"));
    }
    if AuthStore::enabled_admins(&updated) == 0 {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Il doit rester au moins un administrateur actif",
        ));
    }
    s.auth.persist(&updated)?;
    *users = updated;
    drop(users);
    s.auth.purge_user_sessions(&username, None);
    Ok(Json(crate::Message {
        message: format!("Utilisateur « {username} » supprimé."),
    }))
}

#[utoipa::path(get, path = "/api/v1/audit", params(("limit" = Option<usize>, Query, description = "Nombre d'entrées (1–1000, défaut 200)")),
    responses((status = 200, body = [crate::audit::AuditEntry])), tag = "Utilisateurs")]
pub async fn audit_log(
    State(s): State<Shared>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> Json<Vec<crate::audit::AuditEntry>> {
    let limit = q
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    Json(s.audit.tail(limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn temp_store() -> (AuthStore, Option<String>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("vm-auth-{}", uuid::Uuid::new_v4()));
        let (store, pw) = AuthStore::load(&dir, 3600).unwrap();
        (store, pw, dir)
    }

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Viewer < Role::Operator && Role::Operator < Role::Admin);
    }

    #[test]
    fn permission_table() {
        let g = Method::GET;
        let p = Method::POST;
        let d = Method::DELETE;
        assert_eq!(access_for(&g, "/api/v1/health"), Access::Public);
        assert_eq!(access_for(&p, "/api/v1/auth/login"), Access::Public);
        assert_eq!(
            access_for(&g, "/api/v1/auth/login"),
            Access::Role(Role::Viewer)
        );
        assert_eq!(access_for(&g, "/api/v1/vms"), Access::Role(Role::Viewer));
        assert_eq!(access_for(&p, "/api/v1/vms"), Access::Role(Role::Admin));
        let u = "18a09112-5827-45e0-ac41-89f8b3f46635";
        for action in ["start", "shutdown", "suspend", "resume"] {
            assert_eq!(
                access_for(&p, &format!("/api/v1/vms/{u}/{action}")),
                Access::Role(Role::Operator)
            );
        }
        assert_eq!(
            access_for(&g, &format!("/api/v1/vms/{u}/vnc")),
            Access::Role(Role::Operator)
        );
        assert_eq!(
            access_for(&g, &format!("/api/v1/vms/{u}/console")),
            Access::Role(Role::Viewer)
        );
        assert_eq!(
            access_for(&p, &format!("/api/v1/vms/{u}/clone")),
            Access::Role(Role::Admin)
        );
        assert_eq!(
            access_for(&d, &format!("/api/v1/vms/{u}")),
            Access::Role(Role::Admin)
        );
        assert_eq!(access_for(&g, "/api/v1/users"), Access::Role(Role::Admin));
        assert_eq!(access_for(&g, "/api/v1/audit"), Access::Role(Role::Admin));
        assert_eq!(
            access_for(&p, "/api/v1/hypervisors"),
            Access::Role(Role::Admin)
        );
        assert_eq!(
            access_for(&g, "/api/v1/hypervisors"),
            Access::Role(Role::Viewer)
        );
        assert_eq!(
            access_for(&p, "/api/v1/storage/pools/default/refresh"),
            Access::Role(Role::Admin)
        );
        // Une action « start » mal placée ne doit pas ouvrir de droit opérateur.
        assert_eq!(
            access_for(&g, &format!("/api/v1/vms/{u}/start")),
            Access::Role(Role::Viewer)
        );
        assert_eq!(
            access_for(&p, "/api/v1/vms/a/b/start"),
            Access::Role(Role::Admin)
        );
    }

    #[test]
    fn password_hash_roundtrip() {
        let h = hash_password("un-mot-de-passe-solide").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password(&h, "un-mot-de-passe-solide"));
        assert!(!verify_password(&h, "autre-chose-123456"));
        assert!(!verify_password("pas-un-hash", "x"));
    }

    #[test]
    fn username_rules() {
        assert!(valid_username("alice"));
        assert!(valid_username("bob.martin-2"));
        for bad in ["ab", "Alice", "-bob", "a b c", "../x", &"x".repeat(33)] {
            assert!(!valid_username(bad), "{bad}");
        }
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; vm_session=abc123; b=2"),
        );
        assert_eq!(token_from_headers(&h).as_deref(), Some("abc123"));
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("vm_session2=x"));
        assert_eq!(token_from_headers(&h), None);
        let c = session_cookie(true, "t", 60);
        assert!(c.contains("HttpOnly") && c.contains("SameSite=Strict") && c.contains("Secure"));
    }

    #[test]
    fn origin_rules() {
        let allowed = vec!["http://127.0.0.1:5173".to_string()];
        let mk = |origin: Option<&str>, host: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, host.parse().unwrap());
            if let Some(o) = origin {
                h.insert(header::ORIGIN, o.parse().unwrap());
            }
            h
        };
        assert!(origin_allowed(&allowed, &mk(None, "127.0.0.1:8080")));
        assert!(origin_allowed(
            &allowed,
            &mk(Some("http://127.0.0.1:8080"), "127.0.0.1:8080")
        ));
        assert!(origin_allowed(
            &allowed,
            &mk(Some("http://127.0.0.1:5173"), "127.0.0.1:8080")
        ));
        assert!(!origin_allowed(
            &allowed,
            &mk(Some("http://evil.example"), "127.0.0.1:8080")
        ));
        assert!(!origin_allowed(
            &allowed,
            &mk(Some("null"), "127.0.0.1:8080")
        ));
        assert!(!origin_allowed(
            &allowed,
            &mk(Some("http://127.0.0.1:8081"), "127.0.0.1:8080")
        ));
    }

    #[test]
    fn bootstrap_login_sessions_and_lockout() {
        let (store, pw, dir) = temp_store();
        let pw = pw.expect("mot de passe initial généré");
        assert!(dir.join("admin-initial-password.txt").exists());
        assert!(store
            .check_credentials("admin", "faux-mot-de-passe")
            .is_none());
        assert!(store.check_credentials("inconnu", &pw).is_none());
        let user = store.check_credentials("admin", &pw).expect("connexion");
        assert_eq!(user.role, Role::Admin);
        let token = store.create_session("admin");
        assert_eq!(store.user_for_token(&token).unwrap().username, "admin");
        assert!(store.user_for_token("jeton-invalide").is_none());
        store.destroy_session(&token);
        assert!(store.user_for_token(&token).is_none());

        for _ in 0..MAX_FAILS_BEFORE_LOCK {
            assert!(store.locked_for("1.2.3.4|admin").is_none());
            store.register_failure("1.2.3.4|admin");
        }
        assert!(store.locked_for("1.2.3.4|admin").is_some());
        assert!(store.locked_for("5.6.7.8|admin").is_none());
        store.clear_failures("1.2.3.4|admin");
        assert!(store.locked_for("1.2.3.4|admin").is_none());

        // Rechargement depuis le disque : plus de nouveau mot de passe généré.
        let (again, pw2) = AuthStore::load(&dir, 60).unwrap();
        assert!(pw2.is_none());
        assert!(again.check_credentials("admin", &pw).is_some());
        let raw = std::fs::read_to_string(dir.join("users.json")).unwrap();
        assert!(
            !raw.contains(&pw),
            "le mot de passe ne doit jamais être stocké en clair"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn disabled_users_lose_access() {
        let (store, pw, dir) = temp_store();
        let pw = pw.unwrap();
        let token = store.create_session("admin");
        store.users.lock().unwrap()[0].disabled = true;
        assert!(store.user_for_token(&token).is_none());
        assert!(store.check_credentials("admin", &pw).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn last_admin_is_counted() {
        let (store, _pw, dir) = temp_store();
        let mut users = store.users.lock().unwrap().clone();
        assert_eq!(AuthStore::enabled_admins(&users), 1);
        users[0].disabled = true;
        assert_eq!(AuthStore::enabled_admins(&users), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
