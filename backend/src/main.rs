//! Virt Manager — API REST libvirt (v1.0.0) : authentification, rôles, multi-hyperviseurs,
//! stockage, console VNC et export IaC. Sert aussi le front-end compilé (frontend/dist).
mod audit;
mod auth;
mod config;
mod console;
mod hypervisors;
mod management;
mod provisioning;
mod storage;
mod templates;
mod util;

use axum::{
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    middleware,
    routing::{any, delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc};
use tower_http::services::{ServeDir, ServeFile};
use utoipa::{
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
    Modify, OpenApi, ToSchema,
};
use virt::domain::Domain;

pub use hypervisors::AppState;

/// État partagé par toutes les routes.
#[derive(Clone)]
pub struct Shared {
    pub cfg: Arc<config::Config>,
    pub auth: Arc<auth::AuthStore>,
    pub hvs: Arc<hypervisors::HvStore>,
    pub audit: Arc<audit::Audit>,
}

/// Réponse simple `{ "message": "…" }`.
#[derive(Serialize, ToSchema)]
pub struct Message {
    pub message: String,
}

#[derive(Serialize, ToSchema)]
struct Health {
    status: &'static str,
    service: &'static str,
    version: &'static str,
}

#[derive(Serialize, ToSchema)]
struct Vm {
    uuid: String,
    name: String,
    /// Identifiant libvirt, absent lorsque la VM est arrêtée.
    id: Option<u32>,
    state: &'static str,
    vcpus: u32,
    /// Mémoire allouée en Mio.
    memory_mib: u64,
    max_memory_mib: u64,
    /// True only for VMs created/cloned by this application.
    managed: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiError {
    pub error: String,
}

// Un corps JSON vide permet de refuser les POST simples venant d'un formulaire tiers.
// L'API reste réservée au développement local : aucun contrôle d'accès utilisateur.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {}

#[derive(Serialize, ToSchema)]
struct ActionResult {
    uuid: String,
    name: String,
    action: &'static str,
    /// Etat observé juste après la requête (shutdown est asynchrone).
    state: &'static str,
    message: String,
}

pub type ApiErr = (StatusCode, Json<ApiError>);
pub type ApiResult<T> = Result<Json<T>, ApiErr>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VmAction {
    Start,
    Shutdown,
    Suspend,
    Resume,
}

impl VmAction {
    fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Shutdown => "shutdown",
            Self::Suspend => "suspend",
            Self::Resume => "resume",
        }
    }

    fn allowed(self, state: u32) -> bool {
        match self {
            Self::Start => state == 5,    // VIR_DOMAIN_SHUTOFF
            Self::Shutdown => state == 1, // VIR_DOMAIN_RUNNING
            Self::Suspend => state == 1,  // VIR_DOMAIN_RUNNING
            Self::Resume => state == 3,   // VIR_DOMAIN_PAUSED
        }
    }

    fn conflict_message(self, current: &'static str) -> String {
        let instruction = match self {
            Self::Start => "Le démarrage est réservé aux VM arrêtées.",
            Self::Shutdown => "L'arrêt propre exige une VM en cours d'exécution. Reprenez d'abord une VM en pause.",
            Self::Suspend => "La mise en pause exige une VM en cours d'exécution.",
            Self::Resume => "La reprise exige une VM en pause.",
        };
        format!("État actuel : {current}. {instruction}")
    }
}

struct SecurityAddon;
impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(auth::SESSION_COOKIE))),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    paths(health, list_vms, start_vm, shutdown_vm, suspend_vm, resume_vm,
        auth::login, auth::logout, auth::me, auth::change_password,
        auth::list_users, auth::create_user, auth::update_user, auth::delete_user, auth::audit_log,
        hypervisors::list_hypervisors, hypervisors::add_hypervisor, hypervisors::delete_hypervisor,
        hypervisors::test_hypervisor, hypervisors::hypervisor_info,
        provisioning::list_resources, provisioning::create_vm, provisioning::clone_vm, provisioning::delete_vm,
        management::vm_details, management::update_hardware, management::add_disk, management::detach_disk, management::resize_disk,
        storage::list_pools, storage::start_pool, storage::stop_pool, storage::refresh_pool, storage::set_autostart,
        storage::create_pool, storage::delete_pool, storage::list_volumes, storage::create_volume,
        storage::delete_volume, storage::upload_iso, storage::set_media,
        templates::list_templates, templates::get_template, templates::export_vm,
        console::console_info),
    components(schemas(Health, Vm, ApiError, ActionRequest, ActionResult, Message,
        provisioning::PoolItem, provisioning::IsoItem, provisioning::Resources,
        provisioning::CreateVmRequest, provisioning::CloneVmRequest, provisioning::DeleteVmRequest,
        provisioning::ProvisionResult, provisioning::DeleteResult,
        management::VmDisk, management::VmDetails, management::HardwareRequest, management::NewDiskRequest,
        management::ResizeDiskRequest, management::DetachDiskRequest, management::ManagementResult,
        console::ConsoleInfo,
        auth::Role, auth::CurrentUser, auth::UserView, auth::LoginRequest, auth::ChangePasswordRequest,
        auth::NewUserRequest, auth::UpdateUserRequest, audit::AuditEntry,
        hypervisors::HvView, hypervisors::NewHypervisor, hypervisors::HypervisorInfo, hypervisors::TestResult,
        storage::PoolDetail, storage::VolumeDetail, storage::NewPoolRequest, storage::ConfirmRequest,
        storage::AutostartRequest, storage::NewVolumeRequest, storage::MediaRequest,
        templates::TemplateInfo, templates::TemplateFile)),
    modifiers(&SecurityAddon),
    security(("session_cookie" = [])),
    tags(
        (name = "Systeme", description = "Etat du serveur REST"),
        (name = "Authentification", description = "Connexion par cookie de session HttpOnly"),
        (name = "Utilisateurs", description = "Comptes, rôles (viewer/operator/admin) et journal d'audit — administrateurs"),
        (name = "Hyperviseurs", description = "Hôtes libvirt locaux et distants (en-tête X-Hypervisor)"),
        (name = "VM", description = "Inventaire et cycle de vie des machines virtuelles"),
        (name = "Stockage", description = "Pools, volumes, ISO et médias"),
        (name = "Console", description = "État VNC et WebSocket de console"),
        (name = "Modèles IaC", description = "Catalogue et export Terraform / Ansible / Packer")
    ),
    info(title = "Virt Manager API", version = "1.0.0", description = "API Rust/libvirt — authentification, multi-hyperviseurs, stockage, console VNC et export IaC")
)]
struct ApiDoc;

pub fn state_label(code: u32) -> &'static str {
    match code {
        1 => "running",
        2 => "blocked",
        3 => "paused",
        4 => "shutdown",
        5 => "shutoff",
        6 => "crashed",
        7 => "pmsuspended",
        _ => "unknown",
    }
}

pub fn api_error(status: StatusCode, message: impl Into<String>) -> ApiErr {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
}

pub fn libvirt_error(message: String) -> ApiErr {
    api_error(StatusCode::BAD_GATEWAY, message)
}

#[utoipa::path(
    get,
    path = "/api/v1/health",
    responses((status = 200, description = "Serveur operationnel", body = Health)),
    tag = "Systeme"
)]
async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        service: "virt-manager-api",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[utoipa::path(
    get,
    path = "/api/v1/vms",
    responses(
        (status = 200, description = "VM actives et inactives", body = [Vm]),
        (status = 502, description = "Erreur libvirt", body = ApiError)
    ),
    tag = "VM"
)]
async fn list_vms(app: AppState) -> ApiResult<Vec<Vm>> {
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<Vm>, String> {
        let conn = app.open_ro()?;
        let domains = conn
            .list_all_domains(0)
            .map_err(|e| format!("Impossible de lister les VM : {e}"))?;

        let mut vms = Vec::with_capacity(domains.len());
        for domain in domains {
            let name = domain.get_name().map_err(|e| e.to_string())?;
            let info = domain
                .get_info()
                .map_err(|e| format!("Impossible de lire la VM {name} : {e}"))?;

            vms.push(Vm {
                uuid: domain.get_uuid_string().map_err(|e| e.to_string())?,
                name,
                id: domain.get_id(),
                state: state_label(info.state),
                vcpus: info.nr_virt_cpu,
                memory_mib: info.memory / 1024,
                max_memory_mib: info.max_mem / 1024,
                managed: provisioning::managed_domain(&domain),
            });
        }
        vms.sort_by_key(|vm| vm.name.to_lowercase());
        Ok(vms)
    })
    .await
    .map_err(|e| libvirt_error(format!("Erreur du traitement libvirt : {e}")))?
    .map_err(libvirt_error)?;
    Ok(Json(result))
}

async fn run_vm_action(
    app: AppState,
    raw_uuid: String,
    action: VmAction,
) -> ApiResult<ActionResult> {
    // Valider AVANT d'interroger libvirt ; seul un UUID canonique est accepté.
    let uuid = uuid::Uuid::parse_str(&raw_uuid)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "UUID de VM invalide"))?
        .to_string();

    let result = tokio::task::spawn_blocking(move || -> Result<ActionResult, ApiErr> {
        // Connexion en écriture uniquement pour une opération demandée explicitement.
        let conn = app.open_rw().map_err(libvirt_error)?;
        let domain = Domain::lookup_by_uuid_string(&conn, &uuid)
            .map_err(|e| api_error(StatusCode::NOT_FOUND, format!("VM introuvable : {e}")))?;
        let name = domain
            .get_name()
            .map_err(|e| libvirt_error(e.to_string()))?;
        let state = domain
            .get_info()
            .map_err(|e| libvirt_error(e.to_string()))?
            .state;

        if !action.allowed(state) {
            return Err(api_error(
                StatusCode::CONFLICT,
                action.conflict_message(state_label(state)),
            ));
        }

        match action {
            VmAction::Start => {
                domain.create().map_err(|e| libvirt_error(e.to_string()))?;
            }
            VmAction::Shutdown => {
                domain
                    .shutdown()
                    .map_err(|e| libvirt_error(e.to_string()))?;
            }
            VmAction::Suspend => {
                domain.suspend().map_err(|e| libvirt_error(e.to_string()))?;
            }
            VmAction::Resume => {
                domain.resume().map_err(|e| libvirt_error(e.to_string()))?;
            }
        }

        let observed_state = domain
            .get_info()
            .map_err(|e| libvirt_error(e.to_string()))?
            .state;
        let message = match action {
            VmAction::Start => "Démarrage demandé à libvirt.",
            VmAction::Shutdown => "Arrêt propre demandé au système invité (peut prendre du temps).",
            VmAction::Suspend => "Mise en pause demandée à libvirt.",
            VmAction::Resume => "Reprise demandée à libvirt.",
        };
        Ok(ActionResult {
            uuid,
            name,
            action: action.label(),
            state: state_label(observed_state),
            message: message.to_string(),
        })
    })
    .await
    .map_err(|e| libvirt_error(format!("Erreur du traitement libvirt : {e}")))??;
    Ok(Json(result))
}

#[utoipa::path(
    post,
    path = "/api/v1/vms/{uuid}/start",
    params(("uuid" = String, Path, description = "UUID de la VM")),
    request_body = ActionRequest,
    responses(
        (status = 200, description = "Demande acceptee", body = ActionResult),
        (status = 400, description = "UUID invalide", body = ApiError),
        (status = 404, description = "VM introuvable", body = ApiError),
        (status = 409, description = "Etat incompatible", body = ApiError),
        (status = 502, description = "Erreur libvirt", body = ApiError)
    ), tag = "VM"
)]
async fn start_vm(
    app: AppState,
    Path(uuid): Path<String>,
    Json(_request): Json<ActionRequest>,
) -> ApiResult<ActionResult> {
    run_vm_action(app, uuid, VmAction::Start).await
}

#[utoipa::path(
    post,
    path = "/api/v1/vms/{uuid}/shutdown",
    params(("uuid" = String, Path, description = "UUID de la VM")),
    request_body = ActionRequest,
    responses(
        (status = 200, description = "Arret propre demande (asynchrone)", body = ActionResult),
        (status = 404, description = "VM introuvable", body = ApiError),
        (status = 409, description = "Etat incompatible", body = ApiError),
        (status = 502, description = "Erreur libvirt", body = ApiError)
    ), tag = "VM"
)]
async fn shutdown_vm(
    app: AppState,
    Path(uuid): Path<String>,
    Json(_request): Json<ActionRequest>,
) -> ApiResult<ActionResult> {
    run_vm_action(app, uuid, VmAction::Shutdown).await
}

#[utoipa::path(
    post,
    path = "/api/v1/vms/{uuid}/suspend",
    params(("uuid" = String, Path, description = "UUID de la VM")),
    request_body = ActionRequest,
    responses(
        (status = 200, description = "Pause demandee", body = ActionResult),
        (status = 404, description = "VM introuvable", body = ApiError),
        (status = 409, description = "Etat incompatible", body = ApiError),
        (status = 502, description = "Erreur libvirt", body = ApiError)
    ), tag = "VM"
)]
async fn suspend_vm(
    app: AppState,
    Path(uuid): Path<String>,
    Json(_request): Json<ActionRequest>,
) -> ApiResult<ActionResult> {
    run_vm_action(app, uuid, VmAction::Suspend).await
}

#[utoipa::path(
    post,
    path = "/api/v1/vms/{uuid}/resume",
    params(("uuid" = String, Path, description = "UUID de la VM")),
    request_body = ActionRequest,
    responses(
        (status = 200, description = "Reprise demandee", body = ActionResult),
        (status = 404, description = "VM introuvable", body = ApiError),
        (status = 409, description = "Etat incompatible", body = ApiError),
        (status = 502, description = "Erreur libvirt", body = ApiError)
    ), tag = "VM"
)]
async fn resume_vm(
    app: AppState,
    Path(uuid): Path<String>,
    Json(_request): Json<ActionRequest>,
) -> ApiResult<ActionResult> {
    run_vm_action(app, uuid, VmAction::Resume).await
}

async fn openapi_spec() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

async fn api_not_found(Path(_rest): Path<String>) -> ApiErr {
    api_error(StatusCode::NOT_FOUND, "Route inconnue")
}

/// En-têtes de sécurité appliqués à toutes les réponses.
async fn security_headers(mut response: axum::response::Response) -> axum::response::Response {
    let h = response.headers_mut();
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' ws: wss:; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        ),
    ] {
        if let Ok(v) = HeaderValue::from_str(value) {
            h.insert(header::HeaderName::from_static(name), v);
        }
    }
    response
}

fn build_router(shared: Shared) -> Router {
    let mut app = Router::new()
        // — système et authentification
        .route("/api/v1/health", get(health))
        .route("/api/v1/auth/login", post(auth::login))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/auth/me", get(auth::me))
        .route("/api/v1/auth/password", post(auth::change_password))
        // — utilisateurs et audit (admin)
        .route(
            "/api/v1/users",
            get(auth::list_users).post(auth::create_user),
        )
        .route(
            "/api/v1/users/{username}",
            put(auth::update_user).delete(auth::delete_user),
        )
        .route("/api/v1/audit", get(auth::audit_log))
        // — hyperviseurs
        .route(
            "/api/v1/hypervisors",
            get(hypervisors::list_hypervisors).post(hypervisors::add_hypervisor),
        )
        .route(
            "/api/v1/hypervisors/{id}",
            delete(hypervisors::delete_hypervisor),
        )
        .route(
            "/api/v1/hypervisors/{id}/test",
            post(hypervisors::test_hypervisor),
        )
        .route(
            "/api/v1/hypervisors/{id}/info",
            get(hypervisors::hypervisor_info),
        )
        // — machines virtuelles
        .route("/api/v1/vms", get(list_vms).post(provisioning::create_vm))
        .route("/api/v1/resources", get(provisioning::list_resources))
        .route("/api/v1/vms/{uuid}/clone", post(provisioning::clone_vm))
        .route(
            "/api/v1/vms/{uuid}",
            get(management::vm_details).delete(provisioning::delete_vm),
        )
        .route(
            "/api/v1/vms/{uuid}/hardware",
            put(management::update_hardware),
        )
        .route("/api/v1/vms/{uuid}/disks", post(management::add_disk))
        .route(
            "/api/v1/vms/{uuid}/disks/{target}",
            delete(management::detach_disk),
        )
        .route(
            "/api/v1/vms/{uuid}/disks/{target}/resize",
            post(management::resize_disk),
        )
        .route("/api/v1/vms/{uuid}/media", put(storage::set_media))
        .route("/api/v1/vms/{uuid}/console", get(console::console_info))
        .route("/api/v1/vms/{uuid}/vnc", get(console::websocket_vnc))
        .route(
            "/api/v1/vms/{uuid}/export/{tool}",
            get(templates::export_vm),
        )
        .route("/api/v1/vms/{uuid}/start", post(start_vm))
        .route("/api/v1/vms/{uuid}/shutdown", post(shutdown_vm))
        .route("/api/v1/vms/{uuid}/suspend", post(suspend_vm))
        .route("/api/v1/vms/{uuid}/resume", post(resume_vm))
        // — stockage
        .route(
            "/api/v1/storage/pools",
            get(storage::list_pools).post(storage::create_pool),
        )
        .route("/api/v1/storage/pools/{pool}", delete(storage::delete_pool))
        .route(
            "/api/v1/storage/pools/{pool}/start",
            post(storage::start_pool),
        )
        .route(
            "/api/v1/storage/pools/{pool}/stop",
            post(storage::stop_pool),
        )
        .route(
            "/api/v1/storage/pools/{pool}/refresh",
            post(storage::refresh_pool),
        )
        .route(
            "/api/v1/storage/pools/{pool}/autostart",
            put(storage::set_autostart),
        )
        .route(
            "/api/v1/storage/pools/{pool}/volumes",
            get(storage::list_volumes).post(storage::create_volume),
        )
        .route(
            "/api/v1/storage/pools/{pool}/volumes/{volume}",
            delete(storage::delete_volume),
        )
        .route(
            "/api/v1/storage/pools/{pool}/isos/{name}",
            put(storage::upload_iso),
        )
        // — modèles IaC et documentation
        .route("/api/v1/templates", get(templates::list_templates))
        .route("/api/v1/templates/{id}", get(templates::get_template))
        .route("/api-docs/openapi.json", get(openapi_spec))
        .route("/api/{*rest}", any(api_not_found));

    if let Some(dir) = &shared.cfg.static_dir {
        // Application monopage : toute URL inconnue renvoie index.html.
        let index = ServeFile::new(dir.join("index.html"));
        app = app.fallback_service(ServeDir::new(dir).not_found_service(index));
    }
    app.layer(middleware::from_fn_with_state(shared.clone(), auth::guard))
        .layer(middleware::map_response(security_headers))
        .with_state(shared)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\nArrêt demandé, fermeture du serveur…");
}

#[tokio::main]
async fn main() {
    let cfg = config::Config::from_env();
    if let Err(e) = std::fs::create_dir_all(&cfg.data_dir) {
        eprintln!("Impossible de créer {} : {e}", cfg.data_dir.display());
        std::process::exit(1);
    }
    let (auth_store, generated_password) =
        auth::AuthStore::load(&cfg.data_dir, cfg.session_ttl_secs).unwrap_or_else(|e| {
            eprintln!("Initialisation de l'authentification impossible : {e}");
            std::process::exit(1);
        });
    let hvs = hypervisors::HvStore::load(&cfg.data_dir, &cfg.local_uri).unwrap_or_else(|e| {
        eprintln!("Chargement des hyperviseurs impossible : {e}");
        std::process::exit(1);
    });
    let shared = Shared {
        audit: Arc::new(audit::Audit::open(&cfg.data_dir)),
        auth: Arc::new(auth_store),
        hvs: Arc::new(hvs),
        cfg: Arc::new(cfg),
    };

    let listener = tokio::net::TcpListener::bind(&shared.cfg.bind)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Impossible d'ouvrir {} : {e}", shared.cfg.bind);
            std::process::exit(1);
        });
    println!("──────────────────────────────────────────────────────────────");
    println!(" Virt Manager v{}", env!("CARGO_PKG_VERSION"));
    println!(" Adresse      : http://{}", shared.cfg.bind);
    println!(" libvirt      : {}", shared.cfg.local_uri);
    println!(" Données      : {}", shared.cfg.data_dir.display());
    match &shared.cfg.static_dir {
        Some(d) => println!(" Interface    : {}", d.display()),
        None => println!(
            " Interface    : absente (API seule — lancez « npm run build » dans frontend/)"
        ),
    }
    if let Some(pw) = &generated_password {
        println!("──────────────────────────────────────────────────────────────");
        println!(" PREMIER LANCEMENT — compte administrateur créé :");
        println!("   identifiant  : admin");
        println!("   mot de passe : {pw}");
        println!(" (à changer dans l'interface ; noté aussi dans admin-initial-password.txt)");
    }
    let loopback = shared
        .cfg
        .bind
        .parse::<SocketAddr>()
        .map(|a| a.ip().is_loopback())
        .unwrap_or(false);
    if !loopback {
        println!("──────────────────────────────────────────────────────────────");
        println!(" ⚠ Écoute hors loopback : le trafic (mots de passe, cookies) n'est PAS chiffré.");
        println!("   Placez un reverse-proxy HTTPS devant et définissez VIRTMGR_COOKIE_SECURE=1.");
    }
    println!("──────────────────────────────────────────────────────────────");

    let app = build_router(shared);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("Erreur du serveur HTTP");
}

#[cfg(test)]
mod tests {
    use super::{state_label, VmAction};

    #[test]
    fn known_libvirt_states_are_mapped() {
        assert_eq!(state_label(1), "running");
        assert_eq!(state_label(5), "shutoff");
        assert_eq!(state_label(3), "paused");
        assert_eq!(state_label(999), "unknown");
    }

    #[test]
    fn actions_require_correct_vm_state() {
        assert!(VmAction::Start.allowed(5));
        assert!(!VmAction::Start.allowed(3));
        assert!(VmAction::Resume.allowed(3));
        assert!(!VmAction::Shutdown.allowed(3));
        assert!(VmAction::Shutdown.allowed(1));
        assert!(VmAction::Suspend.allowed(1));
        assert!(!VmAction::Suspend.allowed(5));
    }
}
