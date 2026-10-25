# Virt Manager — modèle Packer : image Debian 13 qcow2 installée automatiquement (preseed).
#
# Prérequis : Packer >= 1.9, qemu-system-x86, accès à /dev/kvm.
# Utilisation :
#   packer init .
#   packer build \
#     -var 'iso_url=https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/<NOM-DE-L-ISO>.iso' \
#     -var 'iso_checksum=file:https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/SHA256SUMS' \
#     -var 'root_password=<mot-de-passe-identique-a-http/preseed.cfg>' .
#
# Le nom exact de l'ISO « netinst » courante se lit sur
# https://cdimage.debian.org/debian-cd/current/amd64/iso-cd/ (il change à chaque version corrective).
# L'image produite est dans output-debian13/ ; importez-la ensuite dans un pool libvirt.
packer {
  required_plugins {
    qemu = {
      version = ">= 1.1.0"
      source  = "github.com/hashicorp/qemu"
    }
  }
}

variable "iso_url" {
  type = string
}

variable "iso_checksum" {
  type = string
}

variable "root_password" {
  type      = string
  sensitive = true
}

variable "cpus" {
  type    = number
  default = 2
}

variable "memory_mib" {
  type    = number
  default = 2048
}

variable "disk_size" {
  type    = string
  default = "20G"
}

source "qemu" "debian13" {
  vm_name          = "debian13.qcow2"
  output_directory = "output-debian13"
  iso_url          = var.iso_url
  iso_checksum     = var.iso_checksum

  accelerator    = "kvm"
  cpus           = var.cpus
  memory         = var.memory_mib
  disk_size      = var.disk_size
  format         = "qcow2"
  disk_interface = "virtio"
  net_device     = "virtio-net"
  headless       = true

  http_directory = "http"
  boot_wait      = "5s"
  # Installateur Debian en BIOS : Échap ouvre l'invite « boot: ». À adapter si l'ISO démarre en UEFI.
  boot_command = [
    "<esc><wait>",
    "auto url=http://{{ .HTTPIP }}:{{ .HTTPPort }}/preseed.cfg<enter>"
  ]

  communicator     = "ssh"
  ssh_username     = "root"
  ssh_password     = var.root_password
  ssh_timeout      = "45m"
  shutdown_command = "shutdown -P now"
}

build {
  sources = ["source.qemu.debian13"]

  provisioner "shell" {
    inline = [
      "apt-get update",
      "apt-get -y upgrade",
      # Fermer la connexion root par mot de passe une fois l'image construite.
      "sed -i 's/^PermitRootLogin.*/PermitRootLogin prohibit-password/' /etc/ssh/sshd_config"
    ]
  }
}
