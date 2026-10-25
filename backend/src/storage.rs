//! Incrément 4 — gestion des pools, volumes et ISO.
//!
//! Toutes les écritures passent par libvirt (aucune commande shell, aucun chemin de fichier
//! arbitraire fourni par le client). Les volumes utilisés par une VM ne sont jamais supprimés.
use crate::{
    api_error,
    hypervisors::AppState,
    management::{attr, domain, parse_xml, stopped, to_xml, uuid as parse_uuid},
    provisioning::managed_domain,
    ApiErr, ApiResult, Message, Shared,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    Json,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;
use virt::{connect::Connect, storage_pool::StoragePool, storage_vol::StorageVol, stream::Stream};
use xmltree::{Element, XMLNode};

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Serialize, ToSchema)]
pub struct PoolDetail {
    pub name: String,
    /// Type libvirt (`dir`, `netfs`, `logical`…). Seuls les pools `dir` acceptent les écritures.
    pub pool_type: String,
    pub active: bool,
    pub persistent: bool,
    pub autostart: bool,
    pub target_path: Option<String>,
    pub capacity_bytes: u64,
    pub allocation_bytes: u64,
    pub available_bytes: u64,
    pub volume_count: Option<u32>,
}

#[derive(Serialize, ToSchema)]
pub struct VolumeDetail {
    pub name: String,
    pub path: String,
    pub format: Option<String>,
    /// `iso`, `disk` ou `other`.
    pub kind: String,
    pub capacity_bytes: u64,
    pub allocation_bytes: u64,
    /// Noms des VM qui référencent ce volume.
    pub in_use_by: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPoolRequest {
    pub name: String,
    /// Répertoire absolu, sous une racine autorisée (`VIRTMGR_POOL_ROOTS`).
    pub path: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfirmRequest {
    pub confirm_name: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AutostartRequest {
    pub autostart: bool,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewVolumeRequest {
    pub name: String,
    pub size_gib: u64,
    /// `qcow2` ou `raw`.
    pub format: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaRequest {
    /// Pool et nom de l'ISO à insérer ; les deux à `null` pour éjecter.
    pub iso_pool: Option<String>,
    pub iso_name: Option<String>,
}

fn bad(s: impl Into<String>) -> ApiErr {
    api_error(StatusCode::BAD_REQUEST, s)
}
fn conflict(s: impl Into<String>) -> ApiErr {
    api_error(StatusCode::CONFLICT, s)
}
fn backend(e: impl std::fmt::Display) -> ApiErr {
    crate::libvirt_error(e.to_string())
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiErr> + Send + 'static,
) -> Result<T, ApiErr> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| backend(format!("Erreur du traitement libvirt : {e}")))?
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

pub fn valid_volume_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_iso_name(name: &str) -> bool {
    valid_volume_name(name)
        && name.to_ascii_lowercase().ends_with(".iso")
        && !name.starts_with("virtmgr-")
}

fn valid_new_pool_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Un pool ne peut être créé que strictement sous une racine autorisée, sans `..`.
pub fn validate_pool_path(path: &str, roots: &[String]) -> Result<String, String> {
    if path.len() > 200
        || !path.starts_with('/')
        || path.ends_with('/')
        || path.contains("//")
        || !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
        || path.split('/').any(|seg| seg == ".." || seg == ".")
    {
        return Err(
            "Chemin invalide : absolu, sans « .. », sans « // », caractères [A-Za-z0-9._/-]".into(),
        );
    }
    if roots
        .iter()
        .any(|r| path.len() > r.len() + 1 && path.starts_with(&format!("{r}/")))
    {
        Ok(path.to_string())
    } else {
        Err(format!(
            "Le chemin doit se trouver sous l'une des racines autorisées : {}",
            roots.join(", ")
        ))
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// ---------------------------------------------------------------------------------------------
// Aides libvirt
// ---------------------------------------------------------------------------------------------

fn pool_meta(xml: &str) -> Result<(String, Option<String>), ApiErr> {
    let doc = roxmltree::Document::parse(xml).map_err(backend)?;
    let root = doc.root_element();
    let kind = root.attribute("type").unwrap_or("unknown").to_string();
    let target = root
        .children()
        .find(|n| n.has_tag_name("target"))
        .and_then(|t| t.children().find(|c| c.has_tag_name("path")))
        .and_then(|p| p.text())
        .map(str::to_string);
    Ok((kind, target))
}

fn pool_by_name(conn: &Connect, name: &str) -> Result<StoragePool, ApiErr> {
    if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err(bad("Nom de pool invalide"));
    }
    StoragePool::lookup_by_name(conn, name).map_err(|_| {
        api_error(
            StatusCode::NOT_FOUND,
            format!("Pool « {name} » introuvable"),
        )
    })
}

fn active_dir_pool(conn: &Connect, name: &str) -> Result<StoragePool, ApiErr> {
    let pool = pool_by_name(conn, name)?;
    if !pool.is_active().map_err(backend)? {
        return Err(conflict("Le pool est inactif : démarrez-le d'abord"));
    }
    let (kind, _) = pool_meta(&pool.get_xml_desc(0).map_err(backend)?)?;
    if kind != "dir" {
        return Err(bad("Seuls les pools de type dir acceptent cette opération"));
    }
    Ok(pool)
}

/// Chemin de fichier → noms des VM qui l'utilisent (disques et CD-ROM).
fn usage_map(conn: &Connect) -> Result<HashMap<String, Vec<String>>, ApiErr> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for dom in conn.list_all_domains(0).map_err(backend)? {
        let name = dom.get_name().map_err(backend)?;
        let xml = dom.get_xml_desc(0).map_err(backend)?;
        for path in file_sources(&xml) {
            let users = map.entry(path).or_default();
            if !users.contains(&name) {
                users.push(name.clone());
            }
        }
    }
    Ok(map)
}

fn file_sources(xml: &str) -> Vec<String> {
    roxmltree::Document::parse(xml)
        .map(|doc| {
            doc.descendants()
                .filter(|n| n.has_tag_name("disk"))
                .flat_map(|d| d.children().filter(|c| c.has_tag_name("source")))
                .filter_map(|s| s.attribute("file").map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn volume_detail(
    vol: &StorageVol,
    usage: &HashMap<String, Vec<String>>,
) -> Result<VolumeDetail, ApiErr> {
    let name = vol.get_name().map_err(backend)?;
    let path = vol.get_path().map_err(backend)?;
    let info = vol.get_info().map_err(backend)?;
    let xml = vol.get_xml_desc(0).map_err(backend)?;
    let format = roxmltree::Document::parse(&xml).ok().and_then(|d| {
        d.root_element()
            .children()
            .find(|n| n.has_tag_name("target"))
            .and_then(|t| t.children().find(|c| c.has_tag_name("format")))
            .and_then(|f| f.attribute("type").map(str::to_string))
    });
    let lower = name.to_ascii_lowercase();
    let kind = if lower.ends_with(".iso") {
        "iso"
    } else if matches!(format.as_deref(), Some("qcow2" | "raw")) {
        "disk"
    } else {
        "other"
    };
    Ok(VolumeDetail {
        in_use_by: usage.get(&path).cloned().unwrap_or_default(),
        name,
        path,
        format,
        kind: kind.into(),
        capacity_bytes: info.capacity,
        allocation_bytes: info.allocation,
    })
}

// ---------------------------------------------------------------------------------------------
// Pools
// ---------------------------------------------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/storage/pools", responses((status = 200, body = [PoolDetail]), (status = 502, body = crate::ApiError)), tag = "Stockage")]
pub async fn list_pools(app: AppState) -> ApiResult<Vec<PoolDetail>> {
    let pools = blocking(move || {
        let conn = app.open_ro().map_err(backend)?;
        let mut out = Vec::new();
        for pool in conn.list_all_storage_pools(0).map_err(backend)? {
            let active = pool.is_active().map_err(backend)?;
            let (pool_type, target_path) = pool_meta(&pool.get_xml_desc(0).map_err(backend)?)?;
            let info = pool.get_info().map_err(backend)?;
            out.push(PoolDetail {
                name: pool.get_name().map_err(backend)?,
                pool_type,
                active,
                persistent: pool.is_persistent().unwrap_or(false),
                autostart: pool.get_autostart().unwrap_or(false),
                target_path,
                capacity_bytes: info.capacity,
                allocation_bytes: info.allocation,
                available_bytes: info.available,
                volume_count: if active {
                    pool.num_of_volumes().ok()
                } else {
                    None
                },
            });
        }
        out.sort_by_key(|p| p.name.to_lowercase());
        Ok(out)
    })
    .await?;
    Ok(Json(pools))
}

#[derive(Clone, Copy)]
enum PoolAction {
    Start,
    Stop,
    Refresh,
}

async fn pool_action(app: AppState, name: String, action: PoolAction) -> ApiResult<Message> {
    let msg = blocking(move || {
        let conn = app.open_rw().map_err(backend)?;
        let pool = pool_by_name(&conn, &name)?;
        let active = pool.is_active().map_err(backend)?;
        match action {
            PoolAction::Start => {
                if active {
                    return Err(conflict("Le pool est déjà actif"));
                }
                pool.create(0).map_err(backend)?;
                Ok(format!("Pool « {name} » démarré."))
            }
            PoolAction::Stop => {
                if !active {
                    return Err(conflict("Le pool est déjà arrêté"));
                }
                let (_, target) = pool_meta(&pool.get_xml_desc(0).map_err(backend)?)?;
                if let Some(target) = target {
                    for dom in conn.list_all_domains(0).map_err(backend)? {
                        if dom.get_info().map_err(backend)?.state == 5 {
                            continue;
                        }
                        let xml = dom.get_xml_desc(0).map_err(backend)?;
                        if file_sources(&xml)
                            .iter()
                            .any(|p| p.starts_with(&format!("{target}/")))
                        {
                            return Err(conflict(format!(
                                "La VM « {} » (non arrêtée) utilise des volumes de ce pool",
                                dom.get_name().unwrap_or_default()
                            )));
                        }
                    }
                }
                pool.destroy().map_err(backend)?;
                Ok(format!(
                    "Pool « {name} » arrêté (les fichiers sont conservés)."
                ))
            }
            PoolAction::Refresh => {
                if !active {
                    return Err(conflict("Démarrez le pool avant de le rafraîchir"));
                }
                pool.refresh(0).map_err(backend)?;
                Ok(format!("Pool « {name} » rafraîchi."))
            }
        }
    })
    .await?;
    Ok(Json(Message { message: msg }))
}

#[utoipa::path(post, path = "/api/v1/storage/pools/{pool}/start", params(("pool" = String, Path)), request_body = crate::ActionRequest, responses((status = 200, body = Message), (status = 409, body = crate::ApiError)), tag = "Stockage")]
pub async fn start_pool(
    app: AppState,
    Path(pool): Path<String>,
    _body: Json<crate::ActionRequest>,
) -> ApiResult<Message> {
    pool_action(app, pool, PoolAction::Start).await
}
#[utoipa::path(post, path = "/api/v1/storage/pools/{pool}/stop", params(("pool" = String, Path)), request_body = crate::ActionRequest, responses((status = 200, body = Message), (status = 409, body = crate::ApiError)), tag = "Stockage")]
pub async fn stop_pool(
    app: AppState,
    Path(pool): Path<String>,
    _body: Json<crate::ActionRequest>,
) -> ApiResult<Message> {
    pool_action(app, pool, PoolAction::Stop).await
}
#[utoipa::path(post, path = "/api/v1/storage/pools/{pool}/refresh", params(("pool" = String, Path)), request_body = crate::ActionRequest, responses((status = 200, body = Message), (status = 409, body = crate::ApiError)), tag = "Stockage")]
pub async fn refresh_pool(
    app: AppState,
    Path(pool): Path<String>,
    _body: Json<crate::ActionRequest>,
) -> ApiResult<Message> {
    pool_action(app, pool, PoolAction::Refresh).await
}

#[utoipa::path(put, path = "/api/v1/storage/pools/{pool}/autostart", params(("pool" = String, Path)), request_body = AutostartRequest, responses((status = 200, body = Message)), tag = "Stockage")]
pub async fn set_autostart(
    app: AppState,
    Path(name): Path<String>,
    Json(req): Json<AutostartRequest>,
) -> ApiResult<Message> {
    let msg = blocking(move || {
        let conn = app.open_rw().map_err(backend)?;
        let pool = pool_by_name(&conn, &name)?;
        pool.set_autostart(req.autostart).map_err(backend)?;
        Ok(format!(
            "Démarrage automatique du pool « {name} » : {}.",
            if req.autostart {
                "activé"
            } else {
                "désactivé"
            }
        ))
    })
    .await?;
    Ok(Json(Message { message: msg }))
}

#[utoipa::path(post, path = "/api/v1/storage/pools", request_body = NewPoolRequest,
    responses((status = 201, body = Message), (status = 400, body = crate::ApiError), (status = 409, body = crate::ApiError)), tag = "Stockage")]
pub async fn create_pool(
    State(shared): State<Shared>,
    app: AppState,
    Json(req): Json<NewPoolRequest>,
) -> Result<(StatusCode, Json<Message>), ApiErr> {
    if !valid_new_pool_name(&req.name) {
        return Err(bad(
            "Nom de pool : 1 à 64 caractères [A-Za-z0-9._-], début alphanumérique",
        ));
    }
    let path = validate_pool_path(&req.path, &shared.cfg.pool_roots).map_err(bad)?;
    let name = req.name;
    let msg = blocking(move || {
        let conn = app.open_rw().map_err(backend)?;
        if StoragePool::lookup_by_name(&conn, &name).is_ok() {
            return Err(conflict(format!("Un pool « {name} » existe déjà")));
        }
        for existing in conn.list_all_storage_pools(0).map_err(backend)? {
            let (_, target) = pool_meta(&existing.get_xml_desc(0).map_err(backend)?)?;
            if target.as_deref() == Some(path.as_str()) {
                return Err(conflict(format!(
                    "Le répertoire {path} est déjà utilisé par le pool « {} »",
                    existing.get_name().unwrap_or_default()
                )));
            }
        }
        let xml = format!(
            "<pool type='dir'><name>{}</name><target><path>{}</path></target></pool>",
            xml_escape(&name),
            xml_escape(&path)
        );
        let pool = StoragePool::define_xml(&conn, &xml, 0).map_err(backend)?;
        let started = pool.build(0).and_then(|_| pool.create(0));
        if let Err(e) = started {
            let _ = pool.undefine();
            return Err(backend(format!("Création du pool refusée : {e}")));
        }
        let _ = pool.set_autostart(true);
        Ok(format!(
            "Pool « {name} » créé sur {path}, démarré et en démarrage automatique."
        ))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(Message { message: msg })))
}

#[utoipa::path(delete, path = "/api/v1/storage/pools/{pool}", params(("pool" = String, Path)), request_body = ConfirmRequest,
    responses((status = 200, body = Message), (status = 409, body = crate::ApiError)), tag = "Stockage")]
pub async fn delete_pool(
    app: AppState,
    Path(name): Path<String>,
    Json(req): Json<ConfirmRequest>,
) -> ApiResult<Message> {
    if req.confirm_name != name {
        return Err(bad(
            "Confirmation invalide : saisissez le nom exact du pool",
        ));
    }
    let msg = blocking(move || {
        let conn = app.open_rw().map_err(backend)?;
        let pool = pool_by_name(&conn, &name)?;
        let (_, target) = pool_meta(&pool.get_xml_desc(0).map_err(backend)?)?;
        if let Some(target) = &target {
            for (path, vms) in usage_map(&conn)? {
                if path.starts_with(&format!("{target}/")) {
                    return Err(conflict(format!(
                        "Pool utilisé par la VM « {} » : détachez ses volumes d'abord",
                        vms.join(", ")
                    )));
                }
            }
        }
        if pool.is_active().map_err(backend)? {
            pool.destroy().map_err(backend)?;
        }
        pool.undefine().map_err(backend)?;
        Ok(format!(
            "Pool « {name} » retiré de libvirt. Les fichiers du répertoire sont conservés."
        ))
    })
    .await?;
    Ok(Json(Message { message: msg }))
}

// ---------------------------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------------------------

#[utoipa::path(get, path = "/api/v1/storage/pools/{pool}/volumes", params(("pool" = String, Path)),
    responses((status = 200, body = [VolumeDetail]), (status = 404, body = crate::ApiError)), tag = "Stockage")]
pub async fn list_volumes(app: AppState, Path(name): Path<String>) -> ApiResult<Vec<VolumeDetail>> {
    let vols = blocking(move || {
        let conn = app.open_ro().map_err(backend)?;
        let pool = pool_by_name(&conn, &name)?;
        if !pool.is_active().map_err(backend)? {
            return Err(conflict("Le pool est inactif : aucun volume à lister"));
        }
        let usage = usage_map(&conn)?;
        let mut out = Vec::new();
        for vol in pool.list_all_volumes(0).map_err(backend)? {
            out.push(volume_detail(&vol, &usage)?);
        }
        out.sort_by_key(|v| v.name.to_lowercase());
        Ok(out)
    })
    .await?;
    Ok(Json(vols))
}

#[utoipa::path(post, path = "/api/v1/storage/pools/{pool}/volumes", params(("pool" = String, Path)), request_body = NewVolumeRequest,
    responses((status = 201, body = Message), (status = 400, body = crate::ApiError), (status = 409, body = crate::ApiError)), tag = "Stockage")]
pub async fn create_volume(
    app: AppState,
    Path(pool_name): Path<String>,
    Json(req): Json<NewVolumeRequest>,
) -> Result<(StatusCode, Json<Message>), ApiErr> {
    if !(1..=2048).contains(&req.size_gib) {
        return Err(bad("Taille : 1–2048 Gio"));
    }
    let format = match req.format.as_str() {
        "qcow2" | "raw" => req.format.clone(),
        _ => return Err(bad("Format : qcow2 ou raw")),
    };
    let mut name = req.name.trim().to_string();
    if !valid_volume_name(&name) || name.starts_with("virtmgr-") {
        return Err(bad(
            "Nom de volume : [A-Za-z0-9._-], sans point initial, préfixe « virtmgr- » réservé",
        ));
    }
    let ext = if format == "qcow2" { ".qcow2" } else { ".img" };
    if !name.to_ascii_lowercase().ends_with(ext) {
        name.push_str(ext);
    }
    let size = req.size_gib * GIB;
    let msg = blocking(move || {
        let conn = app.open_rw().map_err(backend)?;
        let pool = active_dir_pool(&conn, &pool_name)?;
        if StorageVol::lookup_by_name(&pool, &name).is_ok() {
            return Err(conflict(format!("Le volume « {name} » existe déjà")));
        }
        let xml = format!(
            "<volume><name>{}</name><capacity unit='bytes'>{size}</capacity><allocation unit='bytes'>0</allocation><target><format type='{format}'/></target></volume>",
            xml_escape(&name)
        );
        StorageVol::create_xml(&pool, &xml, 0).map_err(backend)?;
        Ok(format!("Volume « {name} » ({format}) créé dans le pool « {pool_name} »."))
    })
    .await?;
    Ok((StatusCode::CREATED, Json(Message { message: msg })))
}

#[utoipa::path(delete, path = "/api/v1/storage/pools/{pool}/volumes/{volume}", params(("pool" = String, Path), ("volume" = String, Path)), request_body = ConfirmRequest,
    responses((status = 200, body = Message), (status = 409, body = crate::ApiError)), tag = "Stockage")]
pub async fn delete_volume(
    app: AppState,
    Path((pool_name, volume)): Path<(String, String)>,
    Json(req): Json<ConfirmRequest>,
) -> ApiResult<Message> {
    if req.confirm_name != volume {
        return Err(bad(
            "Confirmation invalide : saisissez le nom exact du volume",
        ));
    }
    let msg = blocking(move || {
        let conn = app.open_rw().map_err(backend)?;
        let pool = active_dir_pool(&conn, &pool_name)?;
        let vol = StorageVol::lookup_by_name(&pool, &volume)
            .map_err(|_| api_error(StatusCode::NOT_FOUND, "Volume introuvable"))?;
        let path = vol.get_path().map_err(backend)?;
        if let Some(vms) = usage_map(&conn)?.get(&path) {
            return Err(conflict(format!(
                "Volume utilisé par la VM « {} » : détachez-le ou éjectez-le d'abord",
                vms.join(", ")
            )));
        }
        for other in pool.list_all_volumes(0).map_err(backend)? {
            let xml = other.get_xml_desc(0).map_err(backend)?;
            let doc = roxmltree::Document::parse(&xml).map_err(backend)?;
            let backing = doc
                .descendants()
                .find(|n| n.has_tag_name("backingStore"))
                .and_then(|b| b.children().find(|c| c.has_tag_name("path")))
                .and_then(|p| p.text());
            if backing == Some(path.as_str()) {
                return Err(conflict(format!(
                    "Le volume « {} » dépend de celui-ci (backing store)",
                    other.get_name().unwrap_or_default()
                )));
            }
        }
        vol.delete(0).map_err(backend)?;
        Ok(format!("Volume « {volume} » supprimé définitivement."))
    })
    .await?;
    Ok(Json(Message { message: msg }))
}

// ---------------------------------------------------------------------------------------------
// Téléversement d'ISO (flux libvirt, aucune copie dans un répertoire du serveur)
// ---------------------------------------------------------------------------------------------

/// Lit et jette (de façon bornée) la suite du corps d'une requête déjà refusée. Sans cela, un refus
/// précoce coupe la connexion pendant que le client ou un reverse-proxy envoie encore le fichier :
/// le proxy y voit une panne (502) au lieu du vrai message (409, 413…).
async fn drain_body<S>(stream: &mut S)
where
    S: futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin,
{
    const MAX_DRAIN: usize = 128 * 1024 * 1024;
    // Délai d'INACTIVITÉ (et non durée totale) : un navigateur ou un proxy envoie en continu ;
    // un client qui n'envoie rien est renvoyé au bout de quelques secondes.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(5);
    let mut seen = 0_usize;
    while let Ok(Some(Ok(chunk))) = tokio::time::timeout(IDLE, stream.next()).await {
        seen += chunk.len();
        if seen > MAX_DRAIN {
            break;
        }
    }
}

#[utoipa::path(put, path = "/api/v1/storage/pools/{pool}/isos/{name}", params(("pool" = String, Path), ("name" = String, Path)),
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "Contenu brut de l'ISO ; l'en-tête Content-Length est obligatoire"),
    responses((status = 201, body = Message), (status = 400, body = crate::ApiError), (status = 409, body = crate::ApiError), (status = 411, body = crate::ApiError), (status = 413, body = crate::ApiError)), tag = "Stockage")]
pub async fn upload_iso(
    State(shared): State<Shared>,
    app: AppState,
    Path((pool_name, name)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Message>), ApiErr> {
    let mut stream = body.into_data_stream();
    if !valid_iso_name(&name) {
        drain_body(&mut stream).await;
        return Err(bad(
            "Nom d'ISO : [A-Za-z0-9._-], extension .iso, préfixe « virtmgr- » réservé",
        ));
    }
    let Some(len) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    else {
        drain_body(&mut stream).await;
        return Err(api_error(
            StatusCode::LENGTH_REQUIRED,
            "En-tête Content-Length obligatoire",
        ));
    };
    if len == 0 {
        return Err(bad("Fichier vide"));
    }
    if len > shared.cfg.max_upload_bytes {
        drain_body(&mut stream).await;
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "Fichier trop volumineux (limite {} Gio, VIRTMGR_MAX_UPLOAD_GIB)",
                shared.cfg.max_upload_bytes / GIB
            ),
        ));
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(8);
    let (pool_c, name_c) = (pool_name.clone(), name.clone());
    let worker = tokio::task::spawn_blocking(move || -> Result<(), ApiErr> {
        let conn = app.open_rw().map_err(backend)?;
        let pool = active_dir_pool(&conn, &pool_c)?;
        if StorageVol::lookup_by_name(&pool, &name_c).is_ok() {
            return Err(conflict(format!(
                "Un volume « {name_c} » existe déjà dans ce pool"
            )));
        }
        if pool.get_info().map_err(backend)?.available < len {
            return Err(api_error(
                StatusCode::INSUFFICIENT_STORAGE,
                "Espace insuffisant dans le pool",
            ));
        }
        let xml = format!(
            "<volume><name>{}</name><capacity unit='bytes'>{len}</capacity><allocation unit='bytes'>0</allocation><target><format type='raw'/></target></volume>",
            xml_escape(&name_c)
        );
        let vol = StorageVol::create_xml(&pool, &xml, 0).map_err(backend)?;
        let stream = Stream::new(&conn, 0).map_err(backend)?;
        if let Err(e) = vol.upload(&stream, 0, len, 0) {
            let _ = stream.abort();
            let _ = vol.delete(0);
            return Err(backend(e));
        }
        let mut total = 0_u64;
        while let Some(chunk) = rx.blocking_recv() {
            let mut offset = 0;
            while offset < chunk.len() {
                match stream.send(&chunk[offset..]) {
                    Ok(n) if n > 0 => offset += n,
                    other => {
                        let _ = stream.abort();
                        let _ = vol.delete(0);
                        return Err(backend(format!(
                            "Écriture dans le pool interrompue : {}",
                            other
                                .err()
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "0 octet écrit".into())
                        )));
                    }
                }
            }
            total += chunk.len() as u64;
        }
        if total != len {
            let _ = stream.abort();
            let _ = vol.delete(0);
            return Err(conflict(format!(
                "Transfert incomplet ({total} octets reçus sur {len}) : volume partiel supprimé"
            )));
        }
        if let Err(e) = stream.finish() {
            let _ = vol.delete(0);
            return Err(backend(format!("Finalisation du transfert refusée : {e}")));
        }
        let _ = pool.refresh(0);
        Ok(())
    });

    let mut sent = 0_u64;
    let mut body_error = None;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                body_error = Some(format!("Lecture de la requête interrompue : {e}"));
                break;
            }
        };
        sent += chunk.len() as u64;
        if sent > len {
            body_error = Some("Le corps dépasse la taille annoncée par Content-Length".to_string());
            break;
        }
        if tx.send(chunk).await.is_err() {
            break; // le travailleur s'est arrêté : son erreur est reportée ci-dessous
        }
    }
    drop(tx);
    let outcome = worker
        .await
        .map_err(|e| backend(format!("Erreur du traitement libvirt : {e}")))?;
    if let Err(e) = outcome {
        drain_body(&mut stream).await;
        return Err(e);
    }
    if let Some(e) = body_error {
        return Err(bad(e));
    }
    Ok((
        StatusCode::CREATED,
        Json(Message {
            message: format!(
                "ISO « {name} » ({} Mio) ajoutée au pool « {pool_name} ».",
                len / (1024 * 1024)
            ),
        }),
    ))
}

// ---------------------------------------------------------------------------------------------
// Média CD-ROM d'une VM (insertion / éjection d'ISO, VM arrêtée)
// ---------------------------------------------------------------------------------------------

fn child_elems(el: &Element) -> impl Iterator<Item = &Element> {
    el.children.iter().filter_map(|n| match n {
        XMLNode::Element(e) => Some(e),
        _ => None,
    })
}

pub fn set_cdrom_xml(xml: &str, iso_path: Option<&str>, managed: bool) -> Result<String, ApiErr> {
    let mut root = parse_xml(xml)?;
    let devices = root
        .get_mut_child("devices")
        .ok_or_else(|| bad("Élément devices absent"))?;
    let mut found = false;
    for node in devices.children.iter_mut() {
        let XMLNode::Element(d) = node else { continue };
        if d.name != "disk" || attr(d, "device") != Some("cdrom") {
            continue;
        }
        found = true;
        d.children
            .retain(|n| !matches!(n, XMLNode::Element(e) if e.name == "source"));
        if let Some(path) = iso_path {
            d.attributes.insert("type".into(), "file".into());
            let mut src = Element::new("source");
            src.attributes.insert("file".into(), path.into());
            let pos = d
                .children
                .iter()
                .position(|n| matches!(n, XMLNode::Element(e) if e.name == "driver"))
                .map_or(0, |i| i + 1);
            d.children.insert(pos, XMLNode::Element(src));
            if let Some(drv) = d.get_mut_child("driver") {
                drv.attributes.insert("type".into(), "raw".into());
            }
        }
        break;
    }
    if !found {
        let Some(path) = iso_path else {
            return Err(conflict("Cette VM n'a pas de lecteur CD-ROM à vider"));
        };
        if !managed {
            return Err(conflict(
                "Ajout d'un lecteur CD-ROM réservé aux VM créées ou clonées par l'application",
            ));
        }
        let used: Vec<String> = child_elems(devices)
            .filter(|e| e.name == "disk")
            .filter_map(|e| {
                e.get_child("target")
                    .and_then(|t| attr(t, "dev"))
                    .map(str::to_string)
            })
            .collect();
        let target = ('a'..='z')
            .map(|c| format!("sd{c}"))
            .find(|t| !used.contains(t))
            .ok_or_else(|| conflict("Aucun identifiant de lecteur libre"))?;
        let el = format!(
            "<disk type='file' device='cdrom'><driver name='qemu' type='raw'/><source file='{}'/><target dev='{target}' bus='sata'/><readonly/></disk>",
            xml_escape(path)
        );
        devices.children.push(XMLNode::Element(parse_xml(&el)?));
    }
    if iso_path.is_some() {
        // Démarrage sur le CD d'abord (style `<boot dev>`), sans mélanger avec `<boot order>`.
        if let Some(os) = root.get_mut_child("os") {
            let boots: Vec<&Element> = child_elems(os).filter(|e| e.name == "boot").collect();
            if !boots.is_empty() && !boots.iter().any(|b| attr(b, "dev") == Some("cdrom")) {
                let pos = os
                    .children
                    .iter()
                    .position(|n| matches!(n, XMLNode::Element(e) if e.name == "boot"))
                    .unwrap_or(0);
                let mut boot = Element::new("boot");
                boot.attributes.insert("dev".into(), "cdrom".into());
                os.children.insert(pos, XMLNode::Element(boot));
            }
        }
    }
    to_xml(&root)
}

#[utoipa::path(put, path = "/api/v1/vms/{uuid}/media", params(("uuid" = String, Path)), request_body = MediaRequest,
    responses((status = 200, body = Message), (status = 400, body = crate::ApiError), (status = 409, body = crate::ApiError)), tag = "Stockage")]
pub async fn set_media(
    app: AppState,
    Path(raw): Path<String>,
    Json(req): Json<MediaRequest>,
) -> ApiResult<Message> {
    let id = parse_uuid(&raw)?;
    if req.iso_pool.is_some() != req.iso_name.is_some() {
        return Err(bad(
            "iso_pool et iso_name doivent être fournis ensemble (ou tous deux null pour éjecter)",
        ));
    }
    let msg = blocking(move || {
        let conn = app.open_rw().map_err(backend)?;
        let dom = domain(&conn, &id)?;
        stopped(&dom)?;
        let iso_path = match (&req.iso_pool, &req.iso_name) {
            (Some(p), Some(n)) => {
                if !n.to_ascii_lowercase().ends_with(".iso") {
                    return Err(bad("Le volume choisi n'est pas un fichier ISO"));
                }
                let pool = active_dir_pool(&conn, p)?;
                let vol = StorageVol::lookup_by_name(&pool, n)
                    .map_err(|_| bad("ISO introuvable dans le pool"))?;
                Some(vol.get_path().map_err(backend)?)
            }
            _ => None,
        };
        let xml = dom.get_xml_desc(0).map_err(backend)?;
        let new_xml = set_cdrom_xml(&xml, iso_path.as_deref(), managed_domain(&dom))?;
        virt::domain::Domain::define_xml(&conn, &new_xml).map_err(backend)?;
        Ok(match &req.iso_name {
            Some(n) => format!("ISO « {n} » insérée dans le lecteur CD-ROM."),
            None => "Lecteur CD-ROM vidé (ISO éjectée).".to_string(),
        })
    })
    .await?;
    Ok(Json(Message { message: msg }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<String> {
        vec!["/var/lib/libvirt".into(), "/srv".into(), "/mnt".into()]
    }

    #[test]
    fn pool_paths_are_confined_to_allowed_roots() {
        for ok in ["/var/lib/libvirt/images", "/srv/vm-pool", "/mnt/disk1/isos"] {
            assert!(validate_pool_path(ok, &roots()).is_ok(), "{ok}");
        }
        for bad in [
            "/",
            "/etc",
            "/etc/libvirt",
            "/srv",
            "/var/lib/libvirt",
            "/var/lib/libvirt/../../etc",
            "/srv/a/../../etc",
            "srv/x",
            "/srv/x/",
            "/srv//x",
            "/srv/x y",
            "/srv/é",
            "/srv/./x",
            "/srvx/evil",
            "/root/.ssh",
        ] {
            assert!(validate_pool_path(bad, &roots()).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_are_validated() {
        assert!(valid_volume_name("data-01.qcow2"));
        for bad in [
            "",
            ".hidden",
            "a/b",
            "../x",
            "a b",
            "é.iso",
            &"x".repeat(101),
        ] {
            assert!(!valid_volume_name(bad), "{bad}");
        }
        assert!(valid_iso_name("debian-13.iso"));
        assert!(valid_iso_name("Debian.ISO"));
        assert!(!valid_iso_name("debian.img"));
        assert!(!valid_iso_name("virtmgr-x.iso"));
        assert!(valid_new_pool_name("isos"));
        assert!(!valid_new_pool_name("-x"));
        assert!(!valid_new_pool_name("Téléchargements"));
    }

    const XML: &str = "<domain type='kvm'><name>t</name><os><type arch='x86_64'>hvm</type><boot dev='hd'/></os><devices>\
        <disk type='file' device='disk'><driver name='qemu' type='qcow2'/><source file='/p/a.qcow2'/><target dev='vda' bus='virtio'/></disk>\
        <disk type='file' device='cdrom'><driver name='qemu' type='raw'/><target dev='sda' bus='sata'/><readonly/></disk>\
        </devices></domain>";

    #[test]
    fn inserts_and_ejects_iso() {
        let with = set_cdrom_xml(XML, Some("/p/debian.iso"), false).unwrap();
        let doc = roxmltree::Document::parse(&with).unwrap();
        assert!(doc
            .descendants()
            .any(|n| n.has_tag_name("source") && n.attribute("file") == Some("/p/debian.iso")));
        // le CD passe en premier dans l'ordre de démarrage, sans dupliquer le disque
        let boots: Vec<_> = doc
            .descendants()
            .filter(|n| n.has_tag_name("boot"))
            .filter_map(|b| b.attribute("dev"))
            .collect();
        assert_eq!(boots, vec!["cdrom", "hd"]);
        assert!(with.contains("/p/a.qcow2"));
        let ejected = set_cdrom_xml(&with, None, false).unwrap();
        assert!(!ejected.contains("debian.iso"));
        assert!(ejected.contains("cdrom"));
        assert!(ejected.contains("/p/a.qcow2"));
        // idempotent : pas de second <boot dev='cdrom'/>
        let again = set_cdrom_xml(&with, Some("/p/other.iso"), false).unwrap();
        assert_eq!(
            again.matches("dev=\"cdrom\"").count() + again.matches("dev='cdrom'").count(),
            1
        );
    }

    #[test]
    fn adds_cdrom_only_for_managed_vms() {
        let no_cd = XML.replace("<disk type='file' device='cdrom'><driver name='qemu' type='raw'/><target dev='sda' bus='sata'/><readonly/></disk>", "");
        assert!(set_cdrom_xml(&no_cd, Some("/p/x.iso"), false).is_err());
        let added = set_cdrom_xml(&no_cd, Some("/p/x.iso"), true).unwrap();
        assert!(added.contains("sda") && added.contains("/p/x.iso"));
        assert!(set_cdrom_xml(&no_cd, None, true).is_err());
    }

    #[test]
    fn file_sources_cover_disks_and_cdroms() {
        let s = file_sources(XML);
        assert_eq!(s, vec!["/p/a.qcow2".to_string()]);
        let with = set_cdrom_xml(XML, Some("/p/d.iso"), false).unwrap();
        assert_eq!(file_sources(&with).len(), 2);
    }
}
