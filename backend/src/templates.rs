//! Incrément 7 — modèles Terraform / Ansible / Packer.
//!
//! * un catalogue de modèles statiques (dossier `templates/` du dépôt, embarqués dans le binaire) ;
//! * un export « infrastructure as code » généré à partir d'une VM existante.
//!
//! Les exports reconstruisent une VM **équivalente** (mêmes vCPU/RAM/disques/réseau) avec de
//! nouveaux disques vierges : aucune donnée n'est copiée et aucun secret n'est inclus.
use crate::{
    api_error,
    hypervisors::AppState,
    management::{load_details, uuid as parse_uuid, VmDetails},
    ApiErr, ApiResult,
};
use axum::{extract::Path, http::StatusCode, Json};
use serde::Serialize;
use utoipa::ToSchema;

struct Template {
    id: &'static str,
    tool: &'static str,
    title: &'static str,
    description: &'static str,
    filename: &'static str,
    content: &'static str,
}

const CATALOG: &[Template] = &[
    Template {
        id: "terraform-vm",
        tool: "terraform",
        title: "Terraform — VM KVM (main.tf)",
        description: "Une VM avec disque qcow2, réseau et console VNC via le fournisseur dmacvicar/libvirt 0.8.x.",
        filename: "main.tf",
        content: include_str!("../../templates/terraform/main.tf"),
    },
    Template {
        id: "terraform-tfvars",
        tool: "terraform",
        title: "Terraform — variables d'exemple",
        description: "Valeurs à copier dans terraform.tfvars.",
        filename: "terraform.tfvars.example",
        content: include_str!("../../templates/terraform/terraform.tfvars.example"),
    },
    Template {
        id: "ansible-playbook",
        tool: "ansible",
        title: "Ansible — créer et démarrer une VM",
        description: "Playbook idempotent (collection community.libvirt) ; utilise vm.xml.j2.",
        filename: "create-vm.yml",
        content: include_str!("../../templates/ansible/create-vm.yml"),
    },
    Template {
        id: "ansible-xml",
        tool: "ansible",
        title: "Ansible — gabarit XML du domaine",
        description: "Gabarit Jinja2 du domaine libvirt (à placer à côté du playbook).",
        filename: "vm.xml.j2",
        content: include_str!("../../templates/ansible/vm.xml.j2"),
    },
    Template {
        id: "ansible-inventory",
        tool: "ansible",
        title: "Ansible — inventaire",
        description: "Inventaire minimal (hôte local ou hyperviseurs distants).",
        filename: "inventory.ini",
        content: include_str!("../../templates/ansible/inventory.ini"),
    },
    Template {
        id: "packer-debian",
        tool: "packer",
        title: "Packer — image Debian 13 (qcow2)",
        description: "Construction automatisée d'une image Debian via QEMU et preseed.",
        filename: "debian13.pkr.hcl",
        content: include_str!("../../templates/packer/debian13.pkr.hcl"),
    },
    Template {
        id: "packer-preseed",
        tool: "packer",
        title: "Packer — preseed Debian",
        description: "Réponses de l'installateur (à placer dans http/preseed.cfg).",
        filename: "preseed.cfg",
        content: include_str!("../../templates/packer/http/preseed.cfg"),
    },
];

#[derive(Serialize, ToSchema)]
pub struct TemplateInfo {
    pub id: String,
    pub tool: String,
    pub title: String,
    pub description: String,
    pub filename: String,
}

#[derive(Serialize, ToSchema)]
pub struct TemplateFile {
    pub id: String,
    pub tool: String,
    pub filename: String,
    pub content: String,
}

#[utoipa::path(get, path = "/api/v1/templates", responses((status = 200, body = [TemplateInfo])), tag = "Modèles IaC")]
pub async fn list_templates() -> Json<Vec<TemplateInfo>> {
    Json(
        CATALOG
            .iter()
            .map(|t| TemplateInfo {
                id: t.id.into(),
                tool: t.tool.into(),
                title: t.title.into(),
                description: t.description.into(),
                filename: t.filename.into(),
            })
            .collect(),
    )
}

#[utoipa::path(get, path = "/api/v1/templates/{id}", params(("id" = String, Path)),
    responses((status = 200, body = TemplateFile), (status = 404, body = crate::ApiError)), tag = "Modèles IaC")]
pub async fn get_template(Path(id): Path<String>) -> ApiResult<TemplateFile> {
    CATALOG
        .iter()
        .find(|t| t.id == id)
        .map(|t| {
            Json(TemplateFile {
                id: t.id.into(),
                tool: t.tool.into(),
                filename: t.filename.into(),
                content: t.content.into(),
            })
        })
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "Modèle inconnu"))
}

#[utoipa::path(get, path = "/api/v1/vms/{uuid}/export/{tool}", params(("uuid" = String, Path), ("tool" = String, Path, description = "terraform, ansible ou packer")),
    responses((status = 200, body = TemplateFile), (status = 400, body = crate::ApiError), (status = 404, body = crate::ApiError)), tag = "Modèles IaC")]
pub async fn export_vm(
    app: AppState,
    Path((raw, tool)): Path<(String, String)>,
) -> ApiResult<TemplateFile> {
    let id = parse_uuid(&raw)?;
    if !matches!(tool.as_str(), "terraform" | "ansible" | "packer") {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Outil : terraform, ansible ou packer",
        ));
    }
    let (details, dtype) =
        tokio::task::spawn_blocking(move || -> Result<(VmDetails, &'static str), ApiErr> {
            let conn = app.open_ro().map_err(crate::libvirt_error)?;
            let details = load_details(&conn, &id)?;
            Ok((details, crate::provisioning::domain_type(&conn)))
        })
        .await
        .map_err(|e| crate::libvirt_error(e.to_string()))??;
    let safe = safe_name(&details.name);
    let (filename, content) = match tool.as_str() {
        "terraform" => (format!("{safe}.tf"), terraform_for(&details, &safe)),
        "ansible" => (
            format!("{safe}-playbook.yml"),
            ansible_for(&details, &safe, dtype),
        ),
        _ => (format!("{safe}.pkr.hcl"), packer_for(&details, &safe)),
    };
    Ok(Json(TemplateFile {
        id: format!("vm-{tool}"),
        tool,
        filename,
        content,
    }))
}

// ---------------------------------------------------------------------------------------------
// Génération
// ---------------------------------------------------------------------------------------------

/// Nom réutilisable partout (ressources, fichiers, commentaires) : [A-Za-z0-9._-] uniquement.
pub fn safe_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(63)
        .collect();
    let s = s.trim_matches(|c| c == '-' || c == '.').to_string();
    if s.is_empty() {
        "vm".into()
    } else {
        s
    }
}

/// Chaîne HCL entre guillemets, sans interpolation involontaire (`${`, `%{`).
pub fn hcl_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out.replace("${", "$${").replace("%{", "%%{")
}

fn system_disks(d: &VmDetails) -> Vec<(&str, u64, Option<&str>, &str)> {
    d.disks
        .iter()
        .filter(|x| x.device == "disk")
        .map(|x| {
            (
                x.target.as_str(),
                x.capacity_gib.unwrap_or(10).max(1),
                x.pool.as_deref(),
                x.format.as_deref().unwrap_or("qcow2"),
            )
        })
        .collect()
}

pub fn terraform_for(d: &VmDetails, safe: &str) -> String {
    let disks = system_disks(d);
    let pool = disks.iter().find_map(|x| x.2).unwrap_or("default");
    let network = d.networks.first().map(String::as_str).unwrap_or("default");
    let mut out = format!(
        "# Généré par Virt Manager à partir de la VM « {safe} ».\n\
         # Reconstruit une VM équivalente avec des disques VIERGES : les données ne sont pas copiées.\n\
         # Le fournisseur est épinglé en 0.8.x (le schéma a changé en 0.9).\n\
         terraform {{\n  required_providers {{\n    libvirt = {{\n      source  = \"dmacvicar/libvirt\"\n      version = \"~> 0.8.3\"\n    }}\n  }}\n}}\n\n\
         variable \"libvirt_uri\" {{\n  type    = string\n  default = \"qemu:///system\"\n}}\n\n\
         variable \"vm_name\" {{\n  type    = string\n  default = {}\n}}\n\n\
         provider \"libvirt\" {{\n  uri = var.libvirt_uri\n}}\n\n",
        hcl_str(&format!("{safe}-copy"))
    );
    let list: Vec<(String, u64, &str)> = if disks.is_empty() {
        vec![("vda".into(), 10, "qcow2")]
    } else {
        disks.iter().map(|x| (x.0.to_string(), x.1, x.3)).collect()
    };
    for (i, (target, gib, fmt)) in list.iter().enumerate() {
        let fmt = if *fmt == "raw" { "raw" } else { "qcow2" };
        out.push_str(&format!(
            "resource \"libvirt_volume\" \"disk_{i}\" {{\n  name   = \"${{var.vm_name}}-{}.{fmt}\"\n  pool   = {}\n  format = \"{fmt}\"\n  size   = {gib} * 1024 * 1024 * 1024\n}}\n\n",
            safe_name(target),
            hcl_str(pool),
        ));
    }
    out.push_str(&format!(
        "resource \"libvirt_domain\" \"vm\" {{\n  name   = var.vm_name\n  vcpu   = {}\n  memory = {}\n\n",
        d.vcpus.max(1),
        d.memory_mib.max(256)
    ));
    for i in 0..list.len() {
        out.push_str(&format!(
            "  disk {{\n    volume_id = libvirt_volume.disk_{i}.id\n  }}\n\n"
        ));
    }
    out.push_str(&format!(
        "  network_interface {{\n    network_name = {}\n  }}\n\n",
        hcl_str(network)
    ));
    out.push_str(
        "  graphics {\n    type           = \"vnc\"\n    listen_type    = \"address\"\n    listen_address = \"127.0.0.1\"\n    autoport       = true\n  }\n}\n\noutput \"vm_id\" {\n  value = libvirt_domain.vm.id\n}\n",
    );
    out
}

/// Chaîne YAML entre guillemets doubles (la syntaxe JSON est du YAML valide).
fn yaml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

pub fn ansible_for(d: &VmDetails, safe: &str, dtype: &str) -> String {
    let disks = system_disks(d);
    let dir = d
        .disks
        .iter()
        .filter(|x| x.device == "disk")
        .find_map(|x| x.source.as_deref())
        .and_then(|p| p.rsplit_once('/').map(|(dir, _)| dir.to_string()))
        .filter(|p| p.starts_with('/') && !p.contains('{') && !p.contains('}'))
        .unwrap_or_else(|| "/var/lib/libvirt/images".into());
    let network = d.networks.first().map(String::as_str).unwrap_or("default");
    let list: Vec<(String, u64)> = if disks.is_empty() {
        vec![("vda".into(), 10)]
    } else {
        disks.iter().map(|x| (x.0.to_string(), x.1)).collect()
    };
    let mut out = format!(
        "---\n# Généré par Virt Manager à partir de la VM « {safe} ».\n\
         # Recrée une VM équivalente avec des disques VIERGES (aucune donnée copiée).\n\
         # Prérequis : ansible-galaxy collection install community.libvirt ;\n\
         # sur l'hôte : qemu-utils, python3-libvirt, python3-lxml.\n\
         - name: Recréer la VM {safe}\n  hosts: hypervisors\n  become: true\n  gather_facts: false\n  vars:\n"
    );
    out.push_str(&format!(
        "    vm_name: {}\n",
        yaml_str(&format!("{safe}-copy"))
    ));
    out.push_str(&format!(
        "    vm_vcpus: {}\n    vm_memory_mib: {}\n",
        d.vcpus.max(1),
        d.memory_mib.max(256)
    ));
    out.push_str(&format!("    vm_domain_type: {}\n", yaml_str(dtype)));
    out.push_str(&format!("    vm_images_dir: {}\n", yaml_str(&dir)));
    out.push_str(&format!("    vm_network: {}\n", yaml_str(network)));
    out.push_str("    vm_disks:\n");
    for (target, gib) in &list {
        out.push_str(&format!(
            "      - {{ dev: {}, size_gib: {gib} }}\n",
            yaml_str(&safe_name(target))
        ));
    }
    out.push_str("    vm_xml: |\n");
    out.push_str(
        "      <domain type='{{ vm_domain_type }}'>\n        <name>{{ vm_name }}</name>\n",
    );
    out.push_str("        <memory unit='MiB'>{{ vm_memory_mib }}</memory>\n        <vcpu placement='static'>{{ vm_vcpus }}</vcpu>\n");
    out.push_str("        <os><type arch='x86_64'>hvm</type><boot dev='hd'/></os>\n        <features><acpi/><apic/></features>\n        <devices>\n");
    out.push_str("      {% for disk in vm_disks %}\n          <disk type='file' device='disk'>\n            <driver name='qemu' type='qcow2'/>\n            <source file='{{ vm_images_dir }}/{{ vm_name }}-{{ disk.dev }}.qcow2'/>\n            <target dev='{{ disk.dev }}' bus='virtio'/>\n          </disk>\n      {% endfor %}\n");
    out.push_str("          <interface type='network'><source network='{{ vm_network }}'/><model type='virtio'/></interface>\n          <graphics type='vnc' port='-1' autoport='yes' listen='127.0.0.1'/>\n          <video><model type='virtio'/></video>\n          <console type='pty'/>\n        </devices>\n      </domain>\n");
    out.push_str(
        "  tasks:\n\
         \x20   - name: Lister les VM existantes\n      community.libvirt.virt:\n        command: list_vms\n      register: existing_vms\n      changed_when: false\n\n\
         \x20   - name: Créer les disques qcow2\n      ansible.builtin.command:\n        cmd: \"qemu-img create -f qcow2 {{ vm_images_dir }}/{{ vm_name }}-{{ item.dev }}.qcow2 {{ item.size_gib }}G\"\n        creates: \"{{ vm_images_dir }}/{{ vm_name }}-{{ item.dev }}.qcow2\"\n      loop: \"{{ vm_disks }}\"\n      when: vm_name not in existing_vms.list_vms\n\n\
         \x20   - name: Définir la VM\n      community.libvirt.virt:\n        command: define\n        xml: \"{{ vm_xml }}\"\n      when: vm_name not in existing_vms.list_vms\n\n\
         \x20   - name: Démarrer la VM\n      community.libvirt.virt:\n        name: \"{{ vm_name }}\"\n        state: running\n",
    );
    out
}

pub fn packer_for(d: &VmDetails, safe: &str) -> String {
    let disks = system_disks(d);
    let root_gib = disks.first().map(|x| x.1).unwrap_or(20);
    format!(
        "# Généré par Virt Manager à partir de la VM « {safe} ».\n\
         # Squelette Packer/QEMU dimensionné comme la VM d'origine (vCPU, RAM, disque système).\n\
         # Complétez iso_url / iso_checksum et fournissez un fichier http/preseed.cfg\n\
         # (voir le modèle « Packer — preseed Debian » du catalogue).\n\
         packer {{\n  required_plugins {{\n    qemu = {{\n      version = \">= 1.1.0\"\n      source  = \"github.com/hashicorp/qemu\"\n    }}\n  }}\n}}\n\n\
         variable \"iso_url\" {{\n  type = string\n}}\n\n\
         variable \"iso_checksum\" {{\n  type = string\n}}\n\n\
         variable \"root_password\" {{\n  type      = string\n  sensitive = true\n}}\n\n\
         source \"qemu\" \"vm\" {{\n  vm_name          = {}\n  output_directory = {}\n  iso_url          = var.iso_url\n  iso_checksum     = var.iso_checksum\n\n  accelerator    = \"kvm\"\n  cpus           = {}\n  memory         = {}\n  disk_size      = \"{root_gib}G\"\n  format         = \"qcow2\"\n  disk_interface = \"virtio\"\n  net_device     = \"virtio-net\"\n  headless       = true\n\n  http_directory = \"http\"\n  boot_wait      = \"5s\"\n  boot_command = [\n    \"<esc><wait>\",\n    \"auto url=http://{{{{ .HTTPIP }}}}:{{{{ .HTTPPort }}}}/preseed.cfg<enter>\"\n  ]\n\n  communicator     = \"ssh\"\n  ssh_username     = \"root\"\n  ssh_password     = var.root_password\n  ssh_timeout      = \"45m\"\n  shutdown_command = \"shutdown -P now\"\n}}\n\n\
         build {{\n  sources = [\"source.qemu.vm\"]\n}}\n",
        hcl_str(&format!("{safe}.qcow2")),
        hcl_str(&format!("output-{safe}")),
        d.vcpus.max(1),
        d.memory_mib.max(256),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::management::VmDisk;

    fn vm(name: &str) -> VmDetails {
        VmDetails {
            uuid: "18a09112-5827-45e0-ac41-89f8b3f46635".into(),
            name: name.into(),
            state: "shutoff".into(),
            managed: true,
            vcpus: 4,
            memory_mib: 8192,
            disks: vec![
                VmDisk {
                    target: "vda".into(),
                    device: "disk".into(),
                    format: Some("qcow2".into()),
                    source: Some("/srv/pool/a.qcow2".into()),
                    pool: Some("fast".into()),
                    capacity_gib: Some(40),
                    app_managed: true,
                },
                VmDisk {
                    target: "vdb".into(),
                    device: "disk".into(),
                    format: Some("raw".into()),
                    source: Some("/srv/pool/b.img".into()),
                    pool: Some("fast".into()),
                    capacity_gib: Some(100),
                    app_managed: false,
                },
                VmDisk {
                    target: "sda".into(),
                    device: "cdrom".into(),
                    format: Some("raw".into()),
                    source: None,
                    pool: None,
                    capacity_gib: None,
                    app_managed: false,
                },
            ],
            networks: vec!["lan".into()],
            graphics: Some("vnc".into()),
        }
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(safe_name("web 01"), "web-01");
        assert_eq!(safe_name("{{ evil }}\n"), "evil");
        assert_eq!(safe_name("$%{x}"), "x");
        assert_eq!(safe_name("***"), "vm");
        assert_eq!(safe_name(&"a".repeat(100)).len(), 63);
    }

    #[test]
    fn hcl_strings_cannot_interpolate_or_break_out() {
        assert_eq!(hcl_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(hcl_str("${evil}"), "\"$${evil}\"");
        assert_eq!(hcl_str("%{if x}"), "\"%%{if x}\"");
        assert_eq!(hcl_str("l1\nl2"), "\"l1\\nl2\"");
        assert_eq!(hcl_str("back\\slash"), "\"back\\\\slash\"");
    }

    #[test]
    fn terraform_reflects_vm_specs() {
        let t = terraform_for(&vm("web 01"), &safe_name("web 01"));
        assert!(t.contains("vcpu   = 4") && t.contains("memory = 8192"));
        assert!(t.contains("size   = 40 *") && t.contains("size   = 100 *"));
        assert!(t.contains("network_name = \"lan\"") && t.contains("pool   = \"fast\""));
        assert!(t.contains("format = \"raw\""));
        assert!(t.contains("libvirt_volume.disk_1.id"));
        assert!(!t.contains("cdrom"), "le CD-ROM n'est pas recréé");
        assert_eq!(
            t.matches('{').count(),
            t.matches('}').count(),
            "accolades équilibrées"
        );
    }

    #[test]
    fn ansible_is_valid_looking_yaml_with_safe_values() {
        let a = ansible_for(&vm("db"), "db", "qemu");
        assert!(a.starts_with("---\n"));
        assert!(a.contains("vm_vcpus: 4") && a.contains("vm_memory_mib: 8192"));
        assert!(
            a.contains("vm_domain_type: \"qemu\"") && a.contains("vm_images_dir: \"/srv/pool\"")
        );
        assert!(a.contains("{ dev: \"vdb\", size_gib: 100 }"));
        assert!(!a.contains("sda"));
    }

    #[test]
    fn packer_uses_first_disk_size_and_escapes_boot_command() {
        let p = packer_for(&vm("x"), "x");
        assert!(p.contains("disk_size      = \"40G\"") && p.contains("cpus           = 4"));
        assert!(p.contains("{{ .HTTPIP }}") && !p.contains("{{{{"));
        assert_eq!(p.matches('{').count(), p.matches('}').count());
    }

    #[test]
    fn catalog_is_complete_and_ids_unique() {
        let mut ids: Vec<_> = CATALOG.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
        assert!(CATALOG.iter().all(|t| !t.content.trim().is_empty()));
        for tool in ["terraform", "ansible", "packer"] {
            assert!(CATALOG.iter().any(|t| t.tool == tool), "{tool}");
        }
    }
}
