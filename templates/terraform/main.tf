# Virt Manager — modèle Terraform : une VM KVM avec un disque qcow2, via libvirt.
#
# Prérequis : Terraform >= 1.5 (ou OpenTofu), libvirt sur l'hôte, utilisateur dans le groupe `libvirt`.
# Utilisation :
#   cp terraform.tfvars.example terraform.tfvars   # puis adapter
#   terraform init && terraform plan && terraform apply
#
# Le fournisseur dmacvicar/libvirt est volontairement épinglé en 0.8.x : la version 0.9 a changé
# le schéma des ressources (ce fichier n'est pas valable tel quel avec 0.9).
terraform {
  required_version = ">= 1.5"
  required_providers {
    libvirt = {
      source  = "dmacvicar/libvirt"
      version = "~> 0.8.3"
    }
  }
}

variable "libvirt_uri" {
  type        = string
  default     = "qemu:///system"
  description = "URI libvirt, ex. qemu+ssh://user@hote/system pour un hôte distant"
}

variable "vm_name" {
  type    = string
  default = "lab-terraform-01"
}

variable "vcpus" {
  type    = number
  default = 2
}

variable "memory_mib" {
  type    = number
  default = 2048
}

variable "disk_gib" {
  type    = number
  default = 20
}

variable "pool" {
  type    = string
  default = "default"
}

variable "network" {
  type    = string
  default = "default"
}

variable "iso_path" {
  type        = string
  default     = ""
  description = "Chemin d'une ISO d'installation déjà présente sur l'hôte (vide = aucune)"
}

provider "libvirt" {
  uri = var.libvirt_uri
}

resource "libvirt_volume" "root" {
  name   = "${var.vm_name}-root.qcow2"
  pool   = var.pool
  format = "qcow2"
  size   = var.disk_gib * 1024 * 1024 * 1024
}

resource "libvirt_domain" "vm" {
  name   = var.vm_name
  vcpu   = var.vcpus
  memory = var.memory_mib

  disk {
    volume_id = libvirt_volume.root.id
  }

  # ISO d'installation facultative (à vérifier selon la version du fournisseur).
  dynamic "disk" {
    for_each = var.iso_path == "" ? [] : [var.iso_path]
    content {
      file = disk.value
    }
  }

  boot_device {
    dev = ["cdrom", "hd"]
  }

  network_interface {
    network_name = var.network
  }

  graphics {
    type           = "vnc"
    listen_type    = "address"
    listen_address = "127.0.0.1"
    autoport       = true
  }
}

output "vm_id" {
  value = libvirt_domain.vm.id
}
