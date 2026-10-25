//! Incrément 4 — configuration hors ligne et disques gérés exclusivement via libvirt.
//! Les VM externes sont consultables mais leurs disques ne sont pas supprimables ici.
use crate::provisioning::managed_domain;
use crate::{api_error, libvirt_error, state_label, ApiErr, ApiResult, AppState};
use axum::{extract::Path, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use virt::{connect::Connect, domain::Domain, storage_pool::StoragePool, storage_vol::StorageVol};
use xmltree::{Element, XMLNode};

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Serialize, ToSchema)]
pub struct VmDisk {
    pub target: String,
    pub device: String,
    pub format: Option<String>,
    pub source: Option<String>,
    pub pool: Option<String>,
    pub capacity_gib: Option<u64>,
    pub app_managed: bool,
}
#[derive(Serialize, ToSchema)]
pub struct VmDetails {
    pub uuid: String,
    pub name: String,
    pub state: String,
    pub managed: bool,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub disks: Vec<VmDisk>,
    pub networks: Vec<String>,
    pub graphics: Option<String>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HardwareRequest {
    pub vcpus: u32,
    pub memory_mib: u64,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewDiskRequest {
    pub pool: String,
    pub disk_gib: u64,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResizeDiskRequest {
    pub disk_gib: u64,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DetachDiskRequest {
    pub delete_disk: bool,
}
#[derive(Serialize, ToSchema)]
pub struct ManagementResult {
    pub name: String,
    pub message: String,
}

fn bad(s: impl Into<String>) -> ApiErr {
    api_error(StatusCode::BAD_REQUEST, s)
}
fn conflict(s: impl Into<String>) -> ApiErr {
    api_error(StatusCode::CONFLICT, s)
}
fn forbidden(s: impl Into<String>) -> ApiErr {
    api_error(StatusCode::FORBIDDEN, s)
}
fn backend(e: impl std::fmt::Display) -> ApiErr {
    libvirt_error(e.to_string())
}
pub(crate) fn uuid(raw: &str) -> Result<String, ApiErr> {
    uuid::Uuid::parse_str(raw)
        .map(|u| u.to_string())
        .map_err(|_| bad("UUID invalide"))
}
pub(crate) fn domain(conn: &Connect, id: &str) -> Result<Domain, ApiErr> {
    Domain::lookup_by_uuid_string(conn, id)
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "VM introuvable"))
}
pub(crate) fn stopped(domain: &Domain) -> Result<(), ApiErr> {
    if domain.get_info().map_err(backend)?.state != 5 {
        return Err(conflict("Éteignez complètement la VM avant de modifier sa configuration (une pause ne suffit pas)"));
    }
    if domain.has_managed_save(0).map_err(backend)? {
        return Err(conflict(
            "Sauvegarde mémoire gérée présente : opération hors ligne refusée",
        ));
    }
    if !domain.list_all_snapshots(0).map_err(backend)?.is_empty() {
        return Err(conflict(
            "Snapshots présents : opération de modification refusée",
        ));
    }
    Ok(())
}
fn managed_stopped(domain: &Domain) -> Result<(), ApiErr> {
    stopped(domain)?;
    if !managed_domain(domain) {
        return Err(forbidden(
            "Gestion des disques réservée aux VM créées ou clonées par l'application",
        ));
    }
    Ok(())
}
pub(crate) fn to_xml(root: &Element) -> Result<String, ApiErr> {
    let mut bytes = Vec::new();
    root.write(&mut bytes).map_err(backend)?;
    String::from_utf8(bytes).map_err(backend)
}
pub(crate) fn parse_xml(xml: &str) -> Result<Element, ApiErr> {
    Element::parse(xml.as_bytes()).map_err(backend)
}
pub(crate) fn attr<'a>(e: &'a Element, key: &str) -> Option<&'a str> {
    e.attributes.get(key).map(String::as_str)
}
fn volume_for_path(conn: &Connect, path: &str) -> Result<(StorageVol, StoragePool), ApiErr> {
    let vol = StorageVol::lookup_by_path(conn, path)
        .map_err(|_| bad("Disque introuvable dans un pool libvirt actif"))?;
    let pool = StoragePool::lookup_by_volume(&vol).map_err(backend)?;
    if !pool.is_active().map_err(backend)? {
        return Err(conflict("Le pool du disque est inactif"));
    }
    let pool_xml = pool.get_xml_desc(0).map_err(backend)?;
    let doc = roxmltree::Document::parse(&pool_xml).map_err(backend)?;
    if doc.root_element().attribute("type") != Some("dir") {
        return Err(bad("Seuls les pools dir sont gérés"));
    }
    Ok((vol, pool))
}
fn managed_extra_name(id: &str, volume: &str) -> bool {
    volume.starts_with(&format!("virtmgr-{id}-extra-"))
        && volume.ends_with(".qcow2")
        && volume.len()
            == format!(
                "virtmgr-{id}-extra-{}.qcow2",
                "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"
            )
            .len()
        && volume
            .strip_prefix(&format!("virtmgr-{id}-extra-"))
            .and_then(|s| s.strip_suffix(".qcow2"))
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .is_some()
}
fn app_disk(id: &str, source: &str) -> bool {
    let filename = source.rsplit('/').next().unwrap_or("");
    filename == format!("virtmgr-{id}.qcow2") || managed_extra_name(id, filename)
}
fn disk_by_target<'a>(root: &'a Element, target: &str) -> Option<&'a Element> {
    root.get_child("devices")?
        .children
        .iter()
        .filter_map(|n| match n {
            XMLNode::Element(e) if e.name == "disk" => Some(e),
            _ => None,
        })
        .find(|d| {
            d.get_child("target").and_then(|t| attr(t, "dev")) == Some(target)
                && attr(d, "device") == Some("disk")
        })
}
fn disk_path(root: &Element, target: &str) -> Result<String, ApiErr> {
    let disk = disk_by_target(root, target).ok_or_else(|| bad("Disque cible introuvable"))?;
    if attr(disk, "type") != Some("file")
        || disk.get_child("driver").and_then(|d| attr(d, "type")) != Some("qcow2")
    {
        return Err(bad("Opération limitée aux disques fichier QCOW2"));
    }
    disk.get_child("source")
        .and_then(|e| attr(e, "file"))
        .map(str::to_string)
        .ok_or_else(|| bad("Disque sans chemin de source"))
}
fn check_shared(conn: &Connect, id: &str, source: &str) -> Result<(), ApiErr> {
    for other in conn.list_all_domains(0).map_err(backend)? {
        if other.get_uuid_string().ok().as_deref() == Some(id) {
            continue;
        }
        let xml = other.get_xml_desc(0).map_err(backend)?;
        let doc = roxmltree::Document::parse(&xml).map_err(backend)?;
        if doc
            .descendants()
            .any(|n| n.has_tag_name("source") && n.attribute("file") == Some(source))
        {
            return Err(conflict("Ce volume est référencé par une autre VM"));
        }
    }
    Ok(())
}
fn no_backing(vol: &StorageVol) -> Result<(), ApiErr> {
    let xml = vol.get_xml_desc(0).map_err(backend)?;
    let doc = roxmltree::Document::parse(&xml).map_err(backend)?;
    if doc
        .root_element()
        .children()
        .any(|n| n.has_tag_name("backingStore") && n.children().any(|c| c.has_tag_name("path")))
    {
        return Err(conflict("Volume avec backing store : opération refusée"));
    }
    Ok(())
}
fn mutate_hardware(xml: &str, req: &HardwareRequest) -> Result<String, ApiErr> {
    if !(1..=32).contains(&req.vcpus) || !(256..=131072).contains(&req.memory_mib) {
        return Err(bad("Limites : 1–32 vCPU, 256–131072 Mio de RAM"));
    }
    let mut root = parse_xml(xml)?;
    // Ne pas écraser une topologie/NUMA/ballooning complexe hors de notre modèle.
    let cpu_changed = root
        .get_child("vcpu")
        .and_then(Element::get_text)
        .and_then(|t| t.parse::<u32>().ok())
        .is_none_or(|x| x != req.vcpus);
    if cpu_changed
        && (root.get_child("vcpus").is_some()
            || root
                .get_child("cpu")
                .and_then(|c| c.get_child("topology"))
                .is_some())
    {
        return Err(conflict(
            "Topologie CPU explicite : modification simplifiée refusée",
        ));
    }
    if root.get_child("maxMemory").is_some()
        || root.get_child("numatune").is_some()
        || root.get_child("memoryBacking").is_some()
        || root.get_child("cputune").is_some()
    {
        return Err(conflict(
            "Configuration mémoire/CPU avancée : modification simplifiée refusée",
        ));
    }
    let new_vcpu = root
        .get_mut_child("vcpu")
        .ok_or_else(|| bad("Élément vcpu absent"))?;
    new_vcpu.children = vec![XMLNode::Text(req.vcpus.to_string())];
    for name in ["memory", "currentMemory"] {
        if let Some(el) = root.get_mut_child(name) {
            el.attributes.insert("unit".into(), "MiB".into());
            el.children = vec![XMLNode::Text(req.memory_mib.to_string())];
        } else if name == "memory" {
            return Err(bad("Élément memory absent"));
        }
    }
    to_xml(&root)
}
fn add_disk_xml(xml: &str, path: &str) -> Result<(String, String), ApiErr> {
    let mut root = parse_xml(xml)?;
    let devices = root
        .get_mut_child("devices")
        .ok_or_else(|| bad("Élément devices absent"))?;
    let data_disk_count=devices.children.iter().filter(|n| matches!(n, XMLNode::Element(e) if e.name=="disk" && attr(e,"device")==Some("disk"))).count();
    let targets: Vec<String> = devices
        .children
        .iter()
        .filter_map(|n| match n {
            XMLNode::Element(e) if e.name == "disk" => e
                .get_child("target")
                .and_then(|t| attr(t, "dev"))
                .map(str::to_string),
            _ => None,
        })
        .collect();
    if data_disk_count >= 8 {
        return Err(conflict("Maximum 8 disques par VM dans cette version"));
    }
    let next = ('b'..='z')
        .map(|c| format!("vd{c}"))
        .find(|t| !targets.contains(t))
        .ok_or_else(|| conflict("Aucun identifiant virtio libre"))?;
    let element=format!("<disk type='file' device='disk'><driver name='qemu' type='qcow2'/><source file='{}'/><target dev='{next}' bus='virtio'/></disk>",
        path.replace('&',"&amp;").replace('<',"&lt;").replace('\'',"&apos;").replace('"',"&quot;"));
    devices
        .children
        .push(XMLNode::Element(parse_xml(&element)?));
    Ok((to_xml(&root)?, next))
}
fn remove_disk_xml(xml: &str, target: &str) -> Result<String, ApiErr> {
    let mut root = parse_xml(xml)?;
    let devices = root
        .get_mut_child("devices")
        .ok_or_else(|| bad("Élément devices absent"))?;
    let old = devices.children.len();
    devices.children.retain(|n| !matches!(n, XMLNode::Element(e) if e.name=="disk" && e.get_child("target").and_then(|t|attr(t,"dev"))==Some(target)));
    if old == devices.children.len() {
        return Err(bad("Disque cible introuvable"));
    }
    to_xml(&root)
}

pub(crate) fn load_details(conn: &Connect, id: &str) -> Result<VmDetails, ApiErr> {
    let dom = domain(conn, id)?;
    let info = dom.get_info().map_err(backend)?;
    let xml = dom.get_xml_desc(0).map_err(backend)?;
    let doc = roxmltree::Document::parse(&xml).map_err(backend)?;
    let mut disks = Vec::new();
    for d in doc.descendants().filter(|n| n.has_tag_name("disk")) {
        let source = d
            .children()
            .find(|n| n.has_tag_name("source"))
            .and_then(|n| n.attribute("file"))
            .map(str::to_owned);
        let target = d
            .children()
            .find(|n| n.has_tag_name("target"))
            .and_then(|n| n.attribute("dev"))
            .unwrap_or("inconnu")
            .to_string();
        let mut pool = None;
        let mut capacity_gib = None;
        if let Some(ref path) = source {
            if let Ok(vol) = StorageVol::lookup_by_path(conn, path) {
                if let Ok(p) = StoragePool::lookup_by_volume(&vol) {
                    pool = p.get_name().ok();
                }
                if let Ok(info) = vol.get_info() {
                    capacity_gib = Some(info.capacity.div_ceil(GIB));
                }
            }
        }
        disks.push(VmDisk {
            target,
            device: d.attribute("device").unwrap_or("unknown").into(),
            format: d
                .children()
                .find(|n| n.has_tag_name("driver"))
                .and_then(|n| n.attribute("type"))
                .map(str::to_owned),
            app_managed: source.as_deref().is_some_and(|p| app_disk(id, p)),
            source,
            pool,
            capacity_gib,
        });
    }
    let networks = doc
        .descendants()
        .filter(|n| n.has_tag_name("interface"))
        .filter_map(|n| n.children().find(|c| c.has_tag_name("source")))
        .filter_map(|s| s.attribute("network").or_else(|| s.attribute("bridge")))
        .map(str::to_owned)
        .collect();
    let graphics = doc
        .descendants()
        .find(|n| n.has_tag_name("graphics"))
        .and_then(|n| n.attribute("type"))
        .map(str::to_owned);
    Ok(VmDetails {
        uuid: id.to_string(),
        name: dom.get_name().map_err(backend)?,
        state: state_label(info.state).into(),
        managed: managed_domain(&dom),
        vcpus: info.nr_virt_cpu,
        memory_mib: info.memory / 1024,
        disks,
        networks,
        graphics,
    })
}

#[utoipa::path(get, path="/api/v1/vms/{uuid}", params(("uuid"=String,Path)),
    responses((status=200,body=VmDetails),(status=404,body=crate::ApiError),(status=502,body=crate::ApiError)), tag="VM")]
pub async fn vm_details(state: AppState, Path(raw): Path<String>) -> ApiResult<VmDetails> {
    let id = uuid(&raw)?;
    let data = tokio::task::spawn_blocking(move || -> Result<VmDetails, ApiErr> {
        let conn = state.open_ro().map_err(backend)?;
        load_details(&conn, &id)
    })
    .await
    .map_err(backend)??;
    Ok(Json(data))
}

#[utoipa::path(put, path="/api/v1/vms/{uuid}/hardware", params(("uuid"=String,Path)), request_body=HardwareRequest,
    responses((status=200,body=ManagementResult),(status=409,body=crate::ApiError),(status=502,body=crate::ApiError)), tag="VM")]
pub async fn update_hardware(
    state: AppState,
    Path(raw): Path<String>,
    Json(req): Json<HardwareRequest>,
) -> ApiResult<ManagementResult> {
    let id = uuid(&raw)?;
    let data = tokio::task::spawn_blocking(move || -> Result<ManagementResult, ApiErr> {
        let conn = state.open_rw().map_err(backend)?;
        let dom = domain(&conn, &id)?;
        stopped(&dom)?;
        let xml = dom.get_xml_desc(0).map_err(backend)?;
        let new_xml = mutate_hardware(&xml, &req)?;
        Domain::define_xml(&conn, &new_xml).map_err(backend)?;
        Ok(ManagementResult {
            name: dom.get_name().map_err(backend)?,
            message:
                "CPU/RAM modifiés dans la configuration persistante ; prochain démarrage requis."
                    .into(),
        })
    })
    .await
    .map_err(backend)??;
    Ok(Json(data))
}

#[utoipa::path(post, path="/api/v1/vms/{uuid}/disks", params(("uuid"=String,Path)), request_body=NewDiskRequest,
    responses((status=200,body=ManagementResult),(status=403,body=crate::ApiError),(status=409,body=crate::ApiError),(status=502,body=crate::ApiError)), tag="Stockage")]
pub async fn add_disk(
    state: AppState,
    Path(raw): Path<String>,
    Json(req): Json<NewDiskRequest>,
) -> ApiResult<ManagementResult> {
    let id = uuid(&raw)?;
    if !(1..=2048).contains(&req.disk_gib) {
        return Err(bad("Taille du disque : 1–2048 Gio"));
    }
    let data=tokio::task::spawn_blocking(move || -> Result<ManagementResult,ApiErr> {
        let conn=state.open_rw().map_err(backend)?;
        let dom=domain(&conn,&id)?; managed_stopped(&dom)?;
        let pool=StoragePool::lookup_by_name(&conn,&req.pool).map_err(|_|bad("Pool introuvable"))?;
        if !pool.is_active().map_err(backend)? { return Err(bad("Pool inactif")); }
        let pd=pool.get_xml_desc(0).map_err(backend)?;
        if roxmltree::Document::parse(&pd).map_err(backend)?.root_element().attribute("type")!=Some("dir") {
            return Err(bad("Le pool doit être de type dir"));
        }
        let original=dom.get_xml_desc(0).map_err(backend)?;
        // Pré-validation : ne pas laisser de volume orphelin en cas de VM non compatible.
        add_disk_xml(&original,"/__virtmgr_preflight__.qcow2")?;
        let volname=format!("virtmgr-{id}-extra-{}.qcow2",uuid::Uuid::new_v4());
        let volxml=format!("<volume><name>{volname}</name><capacity unit='bytes'>{}</capacity><allocation unit='bytes'>0</allocation><target><format type='qcow2'/></target></volume>",req.disk_gib*GIB);
        let vol=StorageVol::create_xml(&pool,&volxml,0).map_err(backend)?;
        let path=match vol.get_path(){Ok(p)=>p,Err(e)=>{let _=vol.delete(0);return Err(backend(e));}};
        let (xml,target)=match add_disk_xml(&original,&path){Ok(x)=>x,Err(e)=>{let _=vol.delete(0);return Err(e);}};
        if let Err(e)=Domain::define_xml(&conn,&xml) {
            let clean=vol.delete(0);
            return Err(backend(format!("Attachement refusé : {e}. Nettoyage volume : {}",if clean.is_ok(){"OK"}else{"ÉCHEC"})));
        }
        Ok(ManagementResult{name:dom.get_name().map_err(backend)?,message:format!("Disque {target} de {} Gio ajouté à l'arrêt dans le pool {}.",req.disk_gib,req.pool)})
    }).await.map_err(backend)??;
    Ok(Json(data))
}

#[utoipa::path(delete, path="/api/v1/vms/{uuid}/disks/{target}", params(("uuid"=String,Path),("target"=String,Path)), request_body=DetachDiskRequest,
    responses((status=200,body=ManagementResult),(status=403,body=crate::ApiError),(status=409,body=crate::ApiError),(status=502,body=crate::ApiError)), tag="Stockage")]
pub async fn detach_disk(
    state: AppState,
    Path((raw, target)): Path<(String, String)>,
    Json(req): Json<DetachDiskRequest>,
) -> ApiResult<ManagementResult> {
    let id = uuid(&raw)?;
    if target == "vda" {
        return Err(forbidden("Le disque système vda ne peut pas être détaché"));
    }
    let data = tokio::task::spawn_blocking(move || -> Result<ManagementResult, ApiErr> {
        let conn = state.open_rw().map_err(backend)?;
        let dom = domain(&conn, &id)?;
        managed_stopped(&dom)?;
        let original = dom.get_xml_desc(0).map_err(backend)?;
        let root = parse_xml(&original)?;
        let path = disk_path(&root, &target)?;
        let (vol, _pool) = volume_for_path(&conn, &path)?;
        if !managed_extra_name(&id, &vol.get_name().map_err(backend)?) {
            return Err(forbidden(
                "Détachement réservé aux disques supplémentaires créés par cette application",
            ));
        }
        if req.delete_disk {
            check_shared(&conn, &id, &path)?;
            no_backing(&vol)?;
        }
        let new_xml = remove_disk_xml(&original, &target)?;
        Domain::define_xml(&conn, &new_xml).map_err(backend)?;
        if req.delete_disk {
            vol.delete(0).map_err(|e| {
                backend(format!(
                    "Disque détaché mais volume encore présent (suppression échouée) : {e}"
                ))
            })?;
        }
        Ok(ManagementResult {
            name: dom.get_name().map_err(backend)?,
            message: if req.delete_disk {
                format!("Disque {target} détaché et volume supprimé définitivement.")
            } else {
                format!("Disque {target} détaché ; volume conservé dans son pool.")
            },
        })
    })
    .await
    .map_err(backend)??;
    Ok(Json(data))
}

#[utoipa::path(post, path="/api/v1/vms/{uuid}/disks/{target}/resize", params(("uuid"=String,Path),("target"=String,Path)), request_body=ResizeDiskRequest,
    responses((status=200,body=ManagementResult),(status=403,body=crate::ApiError),(status=409,body=crate::ApiError),(status=502,body=crate::ApiError)), tag="Stockage")]
pub async fn resize_disk(
    state: AppState,
    Path((raw, target)): Path<(String, String)>,
    Json(req): Json<ResizeDiskRequest>,
) -> ApiResult<ManagementResult> {
    let id = uuid(&raw)?;
    if !(1..=2048).contains(&req.disk_gib) {
        return Err(bad("Taille cible : 1–2048 Gio"));
    }
    let data=tokio::task::spawn_blocking(move || -> Result<ManagementResult,ApiErr> {
        let conn=state.open_rw().map_err(backend)?;
        let dom=domain(&conn,&id)?; managed_stopped(&dom)?;
        let root=parse_xml(&dom.get_xml_desc(0).map_err(backend)?)?;
        let path=disk_path(&root,&target)?;
        let (vol,_pool)=volume_for_path(&conn,&path)?;
        if !app_disk(&id,&path) { return Err(forbidden("Redimensionnement réservé aux volumes gérés par cette application")); }
        check_shared(&conn,&id,&path)?; no_backing(&vol)?;
        let old=vol.get_info().map_err(backend)?.capacity;
        let target_size=req.disk_gib*GIB;
        if target_size<=old { return Err(conflict("Seul l'agrandissement est autorisé ; la taille cible doit dépasser l'actuelle")); }
        vol.resize(target_size,0).map_err(backend)?;
        Ok(ManagementResult{name:dom.get_name().map_err(backend)?,message:format!("Capacité du disque {target} étendue à {} Gio. Agrandissez ensuite la partition et le système de fichiers dans la VM.",req.disk_gib)})
    }).await.map_err(backend)??;
    Ok(Json(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    const XML:&str="<domain type='kvm'><name>test</name><memory unit='KiB'>1048576</memory><currentMemory unit='KiB'>1048576</currentMemory><vcpu placement='static'>1</vcpu><devices><disk type='file' device='disk'><driver type='qcow2'/><source file='/var/lib/libvirt/images/a.qcow2'/><target dev='vda' bus='virtio'/></disk></devices></domain>";
    #[test]
    fn offline_cpu_and_ram_preserve_domain_identity() {
        let new = mutate_hardware(
            XML,
            &HardwareRequest {
                vcpus: 3,
                memory_mib: 4096,
            },
        )
        .unwrap();
        let root = Element::parse(new.as_bytes()).unwrap();
        assert_eq!(
            root.get_child("name").unwrap().get_text().as_deref(),
            Some("test")
        );
        assert_eq!(
            root.get_child("vcpu").unwrap().get_text().as_deref(),
            Some("3")
        );
        assert_eq!(
            root.get_child("memory").unwrap().get_text().as_deref(),
            Some("4096")
        );
        assert_eq!(attr(root.get_child("memory").unwrap(), "unit"), Some("MiB"));
    }
    #[test]
    fn add_remove_disk_xml_without_touching_root() {
        let (xml, target) = add_disk_xml(XML, "/var/lib/libvirt/images/extra.qcow2").unwrap();
        assert_eq!(target, "vdb");
        let root = parse_xml(&xml).unwrap();
        assert_eq!(
            disk_path(&root, "vda").unwrap(),
            "/var/lib/libvirt/images/a.qcow2"
        );
        assert_eq!(
            disk_path(&root, "vdb").unwrap(),
            "/var/lib/libvirt/images/extra.qcow2"
        );
        let clean = remove_disk_xml(&xml, "vdb").unwrap();
        assert!(disk_by_target(&parse_xml(&clean).unwrap(), "vdb").is_none());
    }
    #[test]
    fn extra_names_require_valid_uuid() {
        let id = "18a09112-5827-45e0-ac41-89f8b3f46635";
        assert!(managed_extra_name(id,"virtmgr-18a09112-5827-45e0-ac41-89f8b3f46635-extra-aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa.qcow2"));
        assert!(!managed_extra_name(id, "source.qcow2"));
    }
    #[test]
    fn topology_rejects_unsafe_cpu_change() {
        let xml=XML.replace("<vcpu placement='static'>1</vcpu>","<vcpu placement='static'>1</vcpu><cpu><topology sockets='1' cores='1' threads='1'/></cpu>");
        assert!(mutate_hardware(
            &xml,
            &HardwareRequest {
                vcpus: 2,
                memory_mib: 1024
            }
        )
        .is_err());
    }
}
