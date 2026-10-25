//! Increment 3: managed disks and conservative, offline VM provisioning.
//! All writes are handled by libvirt. No shell commands, arbitrary file paths or root helper.
use axum::{extract::Path, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use virt::{
    connect::Connect, domain::Domain, network::Network, storage_pool::StoragePool,
    storage_vol::StorageVol,
};
use xmltree::{Element, XMLNode};

use crate::{api_error, libvirt_error, ApiErr, ApiResult, AppState};

const MARKER_NS: &str = "urn:virt-manager-app";
const VM_VOLUME_PREFIX: &str = "virtmgr-";

#[derive(Serialize, ToSchema)]
pub struct PoolItem {
    pub name: String,
    pub free_gib: u64,
}

#[derive(Serialize, ToSchema)]
pub struct IsoItem {
    pub pool: String,
    pub name: String,
}

#[derive(Serialize, ToSchema)]
pub struct Resources {
    pub pools: Vec<PoolItem>,
    pub networks: Vec<String>,
    pub isos: Vec<IsoItem>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateVmRequest {
    pub name: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub disk_gib: u64,
    pub pool: String,
    /// Active libvirt virtual network name; null means no NIC.
    pub network: Option<String>,
    /// ISO volume name from a libvirt storage pool, never a user filesystem path.
    pub iso_pool: Option<String>,
    pub iso_name: Option<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneVmRequest {
    pub name: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteVmRequest {
    pub confirm_name: String,
    pub delete_disk: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ProvisionResult {
    pub uuid: String,
    pub name: String,
    pub state: &'static str,
    pub disk: String,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct DeleteResult {
    pub uuid: String,
    pub name: String,
    pub disk_deleted: bool,
    pub message: String,
}

fn bad(message: impl Into<String>) -> ApiErr {
    api_error(StatusCode::BAD_REQUEST, message)
}
fn conflict(message: impl Into<String>) -> ApiErr {
    api_error(StatusCode::CONFLICT, message)
}
fn forbidden(message: impl Into<String>) -> ApiErr {
    api_error(StatusCode::FORBIDDEN, message)
}
fn backend(e: impl std::fmt::Display) -> ApiErr {
    libvirt_error(e.to_string())
}
fn uuid_path(raw: &str) -> Result<String, ApiErr> {
    uuid::Uuid::parse_str(raw)
        .map(|id| id.to_string())
        .map_err(|_| bad("UUID de VM invalide"))
}
fn validate_name(name: &str) -> Result<(), ApiErr> {
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || !name.as_bytes()[0].is_ascii_alphanumeric()
    {
        return Err(bad(
            "Nom invalide : 1 à 63 caractères [A-Za-z0-9_-], début alphanumérique",
        ));
    }
    Ok(())
}
// Unlike VM names, libvirt storage-pool and network names may contain UTF-8
// characters (e.g. "Téléchargements"). They are looked up by name through
// libvirt, not used as paths or shell input; XML attributes are escaped later.
fn validate_resource_name(name: &str, kind: &str) -> Result<(), ApiErr> {
    if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err(bad(format!("Nom de {kind} libvirt invalide")));
    }
    Ok(())
}
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn pool_is_dir(pool: &StoragePool) -> Result<bool, ApiErr> {
    let xml = pool.get_xml_desc(0).map_err(backend)?;
    let document = roxmltree::Document::parse(&xml).map_err(backend)?;
    Ok(document.root_element().attribute("type") == Some("dir"))
}
fn active_pool(conn: &Connect, name: &str) -> Result<StoragePool, ApiErr> {
    validate_resource_name(name, "pool")?;
    let pool = StoragePool::lookup_by_name(conn, name)
        .map_err(|_| bad(format!("Pool libvirt « {name} » inexistant")))?;
    if !pool.is_active().map_err(backend)? || !pool_is_dir(&pool)? {
        return Err(bad("Le pool sélectionné doit être actif et de type dir"));
    }
    Ok(pool)
}
fn check_unique(conn: &Connect, name: &str) -> Result<(), ApiErr> {
    if conn
        .list_all_domains(0)
        .map_err(backend)?
        .iter()
        .any(|domain| domain.get_name().ok().as_deref() == Some(name))
    {
        return Err(conflict(format!("Une VM nommée « {name} » existe déjà")));
    }
    Ok(())
}
fn is_managed_xml(xml: &str) -> bool {
    roxmltree::Document::parse(xml).ok().is_some_and(|doc| {
        doc.descendants()
            .any(|n| n.has_tag_name((MARKER_NS, "managed")))
    })
}
pub fn managed_domain(domain: &Domain) -> bool {
    domain
        .get_xml_desc(0)
        .ok()
        .is_some_and(|xml| is_managed_xml(&xml))
}
fn volume_name(uuid: &str) -> String {
    format!("{VM_VOLUME_PREFIX}{uuid}.qcow2")
}
fn volume_xml(name: &str, bytes: u64) -> String {
    format!("<volume><name>{}</name><capacity unit='bytes'>{bytes}</capacity><allocation unit='bytes'>0</allocation><target><format type='qcow2'/></target></volume>", xml_escape(name))
}
fn marker_xml(pool: &str, volume: &str) -> String {
    format!("<metadata><vmapp:managed xmlns:vmapp='{MARKER_NS}' version='1' pool='{}' volume='{}'/></metadata>", xml_escape(pool), xml_escape(volume))
}

#[utoipa::path(
    get, path = "/api/v1/resources",
    responses((status=200, body=Resources), (status=502, body=crate::ApiError)),
    tag = "Stockage"
)]
pub async fn list_resources(state: AppState) -> ApiResult<Resources> {
    let result = tokio::task::spawn_blocking(move || -> Result<Resources, ApiErr> {
        let conn = state.open_ro().map_err(backend)?;
        let mut pools = Vec::new();
        let mut isos = Vec::new();
        for pool in conn.list_all_storage_pools(0).map_err(backend)? {
            if !pool.is_active().map_err(backend)? || !pool_is_dir(&pool)? {
                continue;
            }
            let name = pool.get_name().map_err(backend)?;
            let info = pool.get_info().map_err(backend)?;
            pools.push(PoolItem {
                name: name.clone(),
                free_gib: info.available / (1024 * 1024 * 1024),
            });
            for vol in pool.list_all_volumes(0).map_err(backend)? {
                let vol_name = vol.get_name().map_err(backend)?;
                if vol_name.to_ascii_lowercase().ends_with(".iso") {
                    isos.push(IsoItem {
                        pool: name.clone(),
                        name: vol_name,
                    });
                }
            }
        }
        let mut networks = Vec::new();
        for net in conn.list_all_networks(0).map_err(backend)? {
            if net.is_active().map_err(backend)? {
                networks.push(net.get_name().map_err(backend)?);
            }
        }
        pools.sort_by(|a, b| a.name.cmp(&b.name));
        networks.sort();
        isos.sort_by(|a, b| (&a.pool, &a.name).cmp(&(&b.pool, &b.name)));
        Ok(Resources {
            pools,
            networks,
            isos,
        })
    })
    .await
    .map_err(backend)??;
    Ok(Json(result))
}

/// `kvm` si l'hôte expose l'accélération matérielle, sinon émulation logicielle `qemu` (TCG).
/// Fonctionne aussi pour un hôte distant : les capacités sont lues via libvirt.
pub fn domain_type(conn: &Connect) -> &'static str {
    let Ok(caps) = conn.get_capabilities() else {
        return "kvm";
    };
    let has_kvm = roxmltree::Document::parse(&caps).is_ok_and(|doc| {
        doc.descendants()
            .any(|n| n.has_tag_name("domain") && n.attribute("type") == Some("kvm"))
    });
    if has_kvm {
        "kvm"
    } else {
        "qemu"
    }
}

fn create_xml(
    request: &CreateVmRequest,
    uuid: &str,
    path: &str,
    iso: Option<&str>,
    dtype: &str,
) -> String {
    let network_xml = request.network.as_ref().map(|name| format!(
        "<interface type='network'><source network='{}'/><model type='virtio'/></interface>", xml_escape(name)
    )).unwrap_or_default();
    let (boot, cdrom) = if let Some(iso) = iso {
        ("<boot dev='cdrom'/><boot dev='hd'/>", format!(
            "<disk type='file' device='cdrom'><driver name='qemu' type='raw'/><source file='{}'/><target dev='sda' bus='sata'/><readonly/></disk>", xml_escape(iso)))
    } else {
        ("<boot dev='hd'/>", String::new())
    };
    format!(
        r#"<domain type='{dtype}'>
<name>{name}</name><uuid>{uuid}</uuid>
{marker}
<memory unit='MiB'>{mem}</memory><currentMemory unit='MiB'>{mem}</currentMemory>
<vcpu placement='static'>{cpu}</vcpu>
<os><type arch='x86_64'>hvm</type>{boot}</os>
<features><acpi/><apic/></features><clock offset='utc'/>
<devices>
<disk type='file' device='disk'><driver name='qemu' type='qcow2'/><source file='{path}'/><target dev='vda' bus='virtio'/></disk>
{cdrom}
{network}
<graphics type='vnc' port='-1' autoport='yes' listen='127.0.0.1'/>
<video><model type='virtio'/></video><console type='pty'/>
</devices></domain>"#,
        dtype = dtype,
        name = xml_escape(&request.name),
        marker = marker_xml(&request.pool, &volume_name(uuid)),
        mem = request.memory_mib,
        cpu = request.vcpus,
        path = xml_escape(path),
        network = network_xml
    )
}

#[utoipa::path(
    post, path="/api/v1/vms", request_body=CreateVmRequest,
    responses((status=201, body=ProvisionResult), (status=400, body=crate::ApiError),
              (status=409, body=crate::ApiError), (status=502, body=crate::ApiError)), tag="VM"
)]
pub async fn create_vm(
    state: AppState,
    Json(request): Json<CreateVmRequest>,
) -> Result<(StatusCode, Json<ProvisionResult>), ApiErr> {
    validate_name(&request.name)?;
    if !(1..=32).contains(&request.vcpus)
        || !(256..=131072).contains(&request.memory_mib)
        || !(1..=2048).contains(&request.disk_gib)
    {
        return Err(bad(
            "Limites : 1–32 vCPU, 256–131072 Mio RAM, 1–2048 Gio disque",
        ));
    }
    if request.iso_pool.is_some() != request.iso_name.is_some() {
        return Err(bad("iso_pool et iso_name doivent être fournis ensemble"));
    }
    let result = tokio::task::spawn_blocking(move || -> Result<ProvisionResult, ApiErr> {
        let conn = state.open_rw().map_err(backend)?;
        check_unique(&conn, &request.name)?;
        let pool = active_pool(&conn, &request.pool)?;
        if let Some(ref name) = request.network {
            validate_resource_name(name, "réseau")?;
            let net = Network::lookup_by_name(&conn, name).map_err(|_| bad("Réseau libvirt introuvable"))?;
            if !net.is_active().map_err(backend)? { return Err(bad("Réseau libvirt inactif")); }
        }
        let iso = match (&request.iso_pool, &request.iso_name) {
            (Some(p), Some(n)) => {
                if !n.to_ascii_lowercase().ends_with(".iso") { return Err(bad("Le volume choisi n'est pas un fichier ISO")); }
                let iso_pool = active_pool(&conn, p)?;
                let vol = StorageVol::lookup_by_name(&iso_pool, n).map_err(|_| bad("ISO introuvable dans le pool"))?;
                Some(vol.get_path().map_err(backend)?)
            }
            _ => None,
        };
        let uuid = uuid::Uuid::new_v4().to_string();
        let disk_name = volume_name(&uuid);
        let bytes = request.disk_gib * 1024 * 1024 * 1024;
        let disk = StorageVol::create_xml(&pool, &volume_xml(&disk_name, bytes), 0).map_err(backend)?;
        let path = match disk.get_path() {
            Ok(p) => p,
            Err(e) => { let _ = disk.delete(0); return Err(backend(e)); }
        };
        let xml = create_xml(&request, &uuid, &path, iso.as_deref(), domain_type(&conn));
        if let Err(e) = Domain::define_xml(&conn, &xml) {
            let cleanup = disk.delete(0);
            return Err(backend(format!("Définition refusée : {e}. Nettoyage du disque : {}", if cleanup.is_ok() { "réussi" } else { "ÉCHEC, vérifier le volume orphelin" })));
        }
        Ok(ProvisionResult { uuid, name: request.name, disk: path, state: "shutoff",
            message: "VM définie à l'arrêt. Le disque est vierge : choisissez un ISO ou clonez une VM installée.".into() })
    }).await.map_err(backend)??;
    Ok((StatusCode::CREATED, Json(result)))
}

fn element(name: &str) -> Element {
    Element::new(name)
}
fn text_el(name: &str, text: &str) -> Element {
    let mut el = element(name);
    el.children.push(XMLNode::Text(text.to_owned()));
    el
}
fn child_mut<'a>(el: &'a mut Element, name: &str) -> Option<&'a mut Element> {
    el.get_mut_child(name)
}
fn remove_child_tags(el: &mut Element, names: &[&str]) {
    el.children.retain(
        |node| !matches!(node, XMLNode::Element(child) if names.contains(&child.name.as_str())),
    );
}
fn child_elements(el: &Element) -> impl Iterator<Item = &Element> {
    el.children.iter().filter_map(|node| match node {
        XMLNode::Element(e) => Some(e),
        _ => None,
    })
}
fn clone_xml(
    source_xml: &str,
    new_name: &str,
    new_uuid: &str,
    new_disk_path: &str,
    pool: &str,
) -> Result<String, ApiErr> {
    let mut root = Element::parse(source_xml.as_bytes()).map_err(backend)?;
    if root.name != "domain" {
        return Err(bad("XML de domaine invalide"));
    }
    if let Some(os) = root.get_child("os") {
        if child_elements(os).any(|n| n.name == "loader" || n.name == "nvram") {
            return Err(bad(
                "Clonage UEFI/NVRAM non pris en charge dans cet incrément",
            ));
        }
    }
    // <pm> only describes guest power-management behavior (suspend-to-mem/disk).
    // It contains no domain-specific identity or shared resource: preserve it as-is.
    // libvirt routinely generates this element even for ordinary BIOS guests.
    let devices = child_mut(&mut root, "devices").ok_or_else(|| bad("Aucun périphérique XML"))?;
    let forbidden_nodes = [
        "tpm",
        "hostdev",
        "filesystem",
        "shmem",
        "smartcard",
        "vsock",
    ];
    if child_elements(devices).any(|n| forbidden_nodes.contains(&n.name.as_str())) {
        return Err(bad(
            "VM avec périphérique partagé/TPM : clonage simplifié interdit",
        ));
    }
    // virt-manager often adds USB redirection channels for SPICE clients.
    // These are virtual, per-guest channels, not USB/PCI host passthrough.
    // Preserve only this safe form, rejecting other redirdev backends and
    // unexpected children (e.g. a remote TCP <source> endpoint).
    if child_elements(devices).any(|n| {
        n.name == "redirdev"
            && !(n.attributes.get("type").map(String::as_str) == Some("spicevmc")
                && n.attributes.get("bus").map(String::as_str) == Some("usb")
                && child_elements(n).all(|c| matches!(c.name.as_str(), "address" | "alias")))
    }) {
        return Err(bad(
            "Redirection USB non-SPICE ou partagée : clonage simplifié interdit",
        ));
    }
    let mut disk_count = 0;
    for device in devices.children.iter_mut().filter_map(|node| match node {
        XMLNode::Element(e) => Some(e),
        _ => None,
    }) {
        if device.name == "disk" {
            if device.attributes.get("device").map(String::as_str) == Some("cdrom") {
                if device.get_child("source").is_some() {
                    return Err(bad("Retirez l'ISO avant de cloner"));
                }
                continue;
            }
            if device.attributes.get("device").map(String::as_str) != Some("disk")
                || device.attributes.get("type").map(String::as_str) != Some("file")
            {
                return Err(bad("Seuls les disques fichier sont clonables"));
            }
            disk_count += 1;
            if disk_count > 1 {
                return Err(bad("Clonage limité aux VM avec un seul disque"));
            }
            let source = child_mut(device, "source").ok_or_else(|| bad("Disque sans source"))?;
            source.attributes.clear();
            source
                .attributes
                .insert("file".into(), new_disk_path.into());
            let driver =
                child_mut(device, "driver").ok_or_else(|| bad("Pilote de disque manquant"))?;
            driver.attributes.insert("type".into(), "qcow2".into());
            remove_child_tags(device, &["alias", "address", "serial", "wwn"]);
        }
        if device.name == "interface" {
            remove_child_tags(device, &["mac", "target", "alias", "address"]);
        }
        if device.name == "graphics" {
            device.attributes.insert("port".into(), "-1".into());
            device.attributes.insert("autoport".into(), "yes".into());
            device
                .attributes
                .insert("listen".into(), "127.0.0.1".into());
            // Avoid retaining a remote graphics listen address.
            remove_child_tags(device, &["listen"]);
        }
    }
    if disk_count != 1 {
        return Err(bad("Un disque système unique est requis"));
    }
    remove_child_tags(
        &mut root,
        &[
            "name", "uuid", "genid", "seclabel", "metadata", "sysinfo", "resource",
        ],
    );
    root.children
        .insert(0, XMLNode::Element(text_el("uuid", new_uuid)));
    root.children
        .insert(0, XMLNode::Element(text_el("name", new_name)));
    root.children.insert(
        2,
        XMLNode::Element(
            Element::parse(marker_xml(pool, &volume_name(new_uuid)).as_bytes()).map_err(backend)?,
        ),
    );
    let mut buffer = Vec::new();
    root.write(&mut buffer).map_err(backend)?;
    String::from_utf8(buffer).map_err(backend)
}
fn root_disk(source_xml: &str) -> Result<String, ApiErr> {
    let doc = roxmltree::Document::parse(source_xml).map_err(backend)?;
    let disks: Vec<_> = doc
        .descendants()
        .filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk"))
        .collect();
    if disks.len() != 1 {
        return Err(bad("Le clonage accepte exactement un disque système"));
    }
    let disk = disks[0];
    if disk.attribute("type") != Some("file") {
        return Err(bad("Disque non-fichier non clonable"));
    }
    let format = disk
        .children()
        .find(|n| n.has_tag_name("driver"))
        .and_then(|n| n.attribute("type"));
    if format != Some("qcow2") && format != Some("raw") {
        return Err(bad("Format source requis : qcow2 ou raw"));
    }
    let path = disk
        .children()
        .find(|n| n.has_tag_name("source"))
        .and_then(|n| n.attribute("file"))
        .ok_or_else(|| bad("Chemin du disque source absent"))?;
    Ok(path.into())
}

#[utoipa::path(
    post, path="/api/v1/vms/{uuid}/clone", params(("uuid" = String, Path, description="VM source arrêtée")),
    request_body=CloneVmRequest,
    responses((status=201, body=ProvisionResult), (status=400, body=crate::ApiError),
              (status=404, body=crate::ApiError), (status=409, body=crate::ApiError),
              (status=502, body=crate::ApiError)), tag="VM"
)]
pub async fn clone_vm(
    state: AppState,
    Path(id): Path<String>,
    Json(request): Json<CloneVmRequest>,
) -> Result<(StatusCode, Json<ProvisionResult>), ApiErr> {
    validate_name(&request.name)?;
    let uuid = uuid_path(&id)?;
    let result = tokio::task::spawn_blocking(move || -> Result<ProvisionResult, ApiErr> {
        let conn = state.open_rw().map_err(backend)?;
        check_unique(&conn, &request.name)?;
        let original = Domain::lookup_by_uuid_string(&conn, &uuid).map_err(|_| api_error(StatusCode::NOT_FOUND, "VM source introuvable"))?;
        if original.get_info().map_err(backend)?.state != 5 {
            return Err(conflict("Arrêtez complètement la VM source (une pause ne suffit pas)"));
        }
        if original.has_managed_save(0).map_err(backend)? {
            return Err(conflict("La VM source possède une sauvegarde mémoire gérée : clonage refusé"));
        }
        if !original.list_all_snapshots(0).map_err(backend)?.is_empty() {
            return Err(conflict("VM source avec snapshots : clonage refusé dans cet incrément"));
        }
        let source_xml = original.get_xml_desc(0).map_err(backend)?;
        let disk_path = root_disk(&source_xml)?;
        let old_vol = StorageVol::lookup_by_path(&conn, &disk_path)
            .map_err(|_| bad("Disque source non géré par un pool libvirt actif"))?;
        let pool = StoragePool::lookup_by_volume(&old_vol).map_err(backend)?;
        if !pool.is_active().map_err(backend)? || !pool_is_dir(&pool)? {
            return Err(bad("Le pool source doit être actif et de type dir"));
        }
        let pool_name = pool.get_name().map_err(backend)?;
        let old_info = old_vol.get_info().map_err(backend)?;
        let new_uuid = uuid::Uuid::new_v4().to_string();
        let new_volume_name = volume_name(&new_uuid);
        // Reject unsupported XML *before* spending time/space copying a volume.
        // The placeholder path is never passed to libvirt; only the resulting
        // validation errors are used here.
        clone_xml(&source_xml, &request.name, &new_uuid,
                  "/__virtmgr_preflight__.qcow2", &pool_name)?;
        let new_disk = StorageVol::create_xml_from(
            &pool, &volume_xml(&new_volume_name, old_info.capacity), &old_vol, 0
        ).map_err(backend)?;
        let new_path = match new_disk.get_path() {
            Ok(p) => p,
            Err(e) => { let _ = new_disk.delete(0); return Err(backend(e)); }
        };
        let xml = match clone_xml(&source_xml, &request.name, &new_uuid, &new_path, &pool_name) {
            Ok(x) => x,
            Err(e) => { let _ = new_disk.delete(0); return Err(e); }
        };
        if let Err(e) = Domain::define_xml(&conn, &xml) {
            let cleanup = new_disk.delete(0);
            return Err(backend(format!("Définition du clone refusée : {e}. Nettoyage : {}", if cleanup.is_ok() { "réussi" } else { "ÉCHEC, vérifier volume orphelin" })));
        }
        Ok(ProvisionResult { uuid: new_uuid, name: request.name, state: "shutoff", disk: new_path,
            message: "Clone à froid défini à l'arrêt ; UUID et MAC régénérés. Vérifiez les identités internes de l'OS avant usage simultané.".into() })
    }).await.map_err(backend)??;
    Ok((StatusCode::CREATED, Json(result)))
}

fn managed_volume(xml: &str, uuid: &str) -> Result<(String, String), ApiErr> {
    let doc = roxmltree::Document::parse(xml).map_err(backend)?;
    let marker = doc
        .descendants()
        .find(|n| n.has_tag_name((MARKER_NS, "managed")))
        .ok_or_else(|| forbidden("Suppression réservée aux VM créées par Virt Manager"))?;
    let pool = marker
        .attribute("pool")
        .ok_or_else(|| forbidden("Marqueur incomplet"))?
        .to_owned();
    let volume = marker
        .attribute("volume")
        .ok_or_else(|| forbidden("Marqueur incomplet"))?
        .to_owned();
    if volume != volume_name(uuid) {
        return Err(forbidden(
            "Nom du volume différent du disque réservé à cette VM",
        ));
    }
    Ok((pool, volume))
}

#[utoipa::path(
    delete, path="/api/v1/vms/{uuid}", params(("uuid" = String, Path, description="VM gérée arrêtée")),
    request_body=DeleteVmRequest,
    responses((status=200, body=DeleteResult), (status=400, body=crate::ApiError),
              (status=403, body=crate::ApiError), (status=404, body=crate::ApiError),
              (status=409, body=crate::ApiError), (status=502, body=crate::ApiError)), tag="VM"
)]
pub async fn delete_vm(
    state: AppState,
    Path(id): Path<String>,
    Json(request): Json<DeleteVmRequest>,
) -> ApiResult<DeleteResult> {
    let uuid = uuid_path(&id)?;
    let result = tokio::task::spawn_blocking(move || -> Result<DeleteResult, ApiErr> {
        let conn = state.open_rw().map_err(backend)?;
        let domain = Domain::lookup_by_uuid_string(&conn, &uuid)
            .map_err(|_| api_error(StatusCode::NOT_FOUND, "VM introuvable"))?;
        let name = domain.get_name().map_err(backend)?;
        if request.confirm_name != name { return Err(bad("Confirmation invalide : saisissez le nom exact de la VM")); }
        if domain.get_info().map_err(backend)?.state != 5 { return Err(conflict("Arrêtez complètement la VM avant suppression")); }
        if domain.has_managed_save(0).map_err(backend)? {
            return Err(conflict("VM avec sauvegarde mémoire gérée : suppression refusée"));
        }
        if !domain.list_all_snapshots(0).map_err(backend)?.is_empty() {
            return Err(conflict("VM avec snapshots : suppression refusée"));
        }
        let xml = domain.get_xml_desc(0).map_err(backend)?;
        let (pool_name, volume_name) = managed_volume(&xml, &uuid)?;
        // Increment 4 may attach extra disks to a VM. Require their removal first
        // so that undefine never loses track of application-managed volumes.
        let disk_count = roxmltree::Document::parse(&xml).map_err(backend)?
            .descendants().filter(|n| n.has_tag_name("disk") && n.attribute("device") == Some("disk")).count();
        if disk_count != 1 {
            return Err(conflict("Détachez les disques supplémentaires dans Détails > Stockage avant de supprimer la VM"));
        }
        let pool = active_pool(&conn, &pool_name)?;
        let volume = StorageVol::lookup_by_name(&pool, &volume_name).map_err(backend)?;
        let disk_path = root_disk(&xml)?;
        if volume.get_path().map_err(backend)? != disk_path {
            return Err(forbidden("Le volume géré ne correspond pas au disque attaché : suppression bloquée"));
        }
        if request.delete_disk {
            for other in conn.list_all_domains(0).map_err(backend)? {
                if other.get_uuid_string().ok().as_deref() == Some(&uuid) { continue; }
                let other_xml = other.get_xml_desc(0).map_err(backend)?;
                if let Ok(doc) = roxmltree::Document::parse(&other_xml) {
                    if doc.descendants().any(|n| n.has_tag_name("source") && n.attribute("file") == Some(disk_path.as_str())) {
                        return Err(conflict("Le disque est aussi attaché à une autre VM"));
                    }
                }
            }
        }
        domain.undefine().map_err(backend)?;
        if request.delete_disk {
            volume.delete(0).map_err(|e| backend(format!("Définition supprimée, mais disque conservé suite à une erreur : {e}")))?;
        }
        Ok(DeleteResult { uuid, name, disk_deleted: request.delete_disk,
            message: if request.delete_disk { "Définition et volume supprimés." } else { "Définition supprimée ; disque conservé dans le pool." }.into() })
    }).await.map_err(backend)??;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_name() {
        for name in ["debian13", "vm_test", "vm-01"] {
            assert!(validate_name(name).is_ok());
        }
        for name in ["", "a/b", "../etc", "vm a", "<bad>"] {
            assert!(validate_name(name).is_err());
        }
    }
    #[test]
    fn resource_names_can_contain_accents_without_weakening_vm_names() {
        assert!(validate_resource_name("Téléchargements", "pool").is_ok());
        assert!(validate_resource_name("réseau-interne", "réseau").is_ok());
        assert!(validate_resource_name("", "pool").is_err());
        assert!(validate_resource_name("pool\nunknown", "pool").is_err());
        assert!(validate_name("clone-source-test").is_ok());
        assert!(validate_name("Téléchargements").is_err());
    }
    #[test]
    fn clone_preserves_virtual_spice_usb_redirections() {
        let source = r#"<domain type='kvm'><name>source</name>
            <uuid>aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa</uuid>
            <pm><suspend-to-mem enabled='no'/><suspend-to-disk enabled='no'/></pm>
            <devices>
              <disk type='file' device='disk'><driver name='qemu' type='qcow2'/>
                <source file='/var/lib/libvirt/images/source.qcow2'/>
                <target dev='vda' bus='virtio'/></disk>
              <interface type='network'><mac address='52:54:00:ff:65:03'/>
                <source network='default'/></interface>
              <graphics type='spice' autoport='yes'><listen type='address'/></graphics>
              <redirdev bus='usb' type='spicevmc'><address type='usb' bus='0' port='2'/></redirdev>
              <redirdev bus='usb' type='spicevmc'><address type='usb' bus='0' port='3'/></redirdev>
            </devices></domain>"#;
        let result = clone_xml(
            source,
            "clone-test",
            "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb",
            "/var/lib/libvirt/images/destination.qcow2",
            "default",
        )
        .unwrap();
        let doc = roxmltree::Document::parse(&result).unwrap();
        assert_eq!(
            doc.descendants()
                .filter(|n| n.has_tag_name("redirdev"))
                .count(),
            2
        );
        assert!(result.contains("destination.qcow2"));
        assert!(!result.contains("source.qcow2"));
        assert!(!result.contains("52:54:00:ff:65:03"));
        assert!(result.contains("spicevmc"));
        assert!(is_managed_xml(&result));
    }
    #[test]
    fn clone_rejects_non_spice_redirects_and_tpm() {
        let prefix = "<domain type='kvm'><name>source</name><devices><disk type='file' device='disk'><driver type='qcow2'/><source file='/tmp/source.qcow2'/></disk>";
        let suffix = "</devices></domain>";
        for unsafe_device in [
            "<redirdev bus='usb' type='tcp'><source mode='connect' host='example.org' service='4000'/></redirdev>",
            "<redirdev bus='usb' type='spicevmc'><source mode='connect' host='example.org'/></redirdev>",
            "<tpm model='tpm-crb'><backend type='emulator' version='2.0'/></tpm>",
            "<hostdev mode='subsystem' type='usb'/>",
        ] {
            let source = format!("{prefix}{unsafe_device}{suffix}");
            assert!(clone_xml(&source, "clone", "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb",
                              "/tmp/dest.qcow2", "default").is_err());
        }
    }
    #[test]
    fn safe_disk_names() {
        assert_eq!(volume_name("abc"), "virtmgr-abc.qcow2");
        assert_eq!(xml_escape("a&'\"<"), "a&amp;&apos;&quot;&lt;");
    }
    #[test]
    fn clone_resets_identity_and_disk() {
        let xml = r#"<domain type='kvm'><name>src</name><uuid>aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa</uuid><devices><disk device='disk' type='file'><driver type='qcow2'/><source file='/tmp/src.qcow2'/><target dev='vda' bus='virtio'/></disk><interface type='network'><mac address='52:54:00:01:02:03'/><source network='default'/></interface></devices></domain>"#;
        let result = clone_xml(
            xml,
            "dest",
            "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb",
            "/tmp/dest.qcow2",
            "default",
        )
        .unwrap();
        assert!(result.contains("dest.qcow2"));
        assert!(!result.contains("src.qcow2"));
        assert!(!result.contains("52:54:00:01:02:03"));
        assert!(is_managed_xml(&result));
    }
    #[test]
    fn clone_preserves_standard_pm_configuration() {
        // libvirt includes <pm> on many ordinary BIOS guests. This must not
        // block an offline clone; the values are safe to retain unchanged.
        let xml = r#"<domain type='kvm'><name>src</name><uuid>aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa</uuid>
            <pm><suspend-to-mem enabled='no'/><suspend-to-disk enabled='no'/></pm>
            <devices><disk device='disk' type='file'><driver type='qcow2'/>
            <source file='/tmp/source.qcow2'/><target dev='vda' bus='virtio'/></disk>
            </devices></domain>"#;
        let result = clone_xml(
            xml,
            "dest",
            "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb",
            "/tmp/destination.qcow2",
            "default",
        )
        .unwrap();
        let doc = roxmltree::Document::parse(&result).unwrap();
        let pm = doc
            .root_element()
            .children()
            .find(|n| n.has_tag_name("pm"))
            .unwrap();
        let settings: Vec<_> = pm.children().filter(|n| n.is_element()).collect();
        assert_eq!(settings.len(), 2);
        assert_eq!(settings[0].tag_name().name(), "suspend-to-mem");
        assert_eq!(settings[0].attribute("enabled"), Some("no"));
        assert_eq!(settings[1].tag_name().name(), "suspend-to-disk");
        assert_eq!(settings[1].attribute("enabled"), Some("no"));
        assert!(result.contains("/tmp/destination.qcow2"));
        assert!(!result.contains("/tmp/source.qcow2"));
    }
    #[test]
    fn external_domain_cannot_be_deleted() {
        assert!(managed_volume("<domain><name>old</name></domain>", "123").is_err());
    }
}
