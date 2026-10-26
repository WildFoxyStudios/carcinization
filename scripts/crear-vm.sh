#!/usr/bin/env bash
# Crea la VM de VirtualBox para Carcinization OS, equivalente a
# `make virtualbox` pero sin arrancar la VM (util para verificar antes).
#
# Uso: crear-vm.sh <ruta-a-harddrive.img>
set -euo pipefail

IMG="${1:?falta la imagen}"
BUILD="$(cd "$(dirname "$IMG")" && pwd)"
VBM="VBoxManage"
VM="Carcinization"

if ! $VBM list vms | grep -q "^\"$VM\""; then
    echo "Creando VM $VM"
    $VBM createvm --name "$VM" --register
else
    echo "La VM $VM ya existe"
fi

echo "Configurando"
$VBM modifyvm "$VM" --memory 2048
$VBM modifyvm "$VM" --vram 32
$VBM modifyvm "$VM" --nic1 nat
$VBM modifyvm "$VM" --nictype1 82540EM
$VBM modifyvm "$VM" --cableconnected1 on
$VBM modifyvm "$VM" --uart1 0x3F8 4
$VBM modifyvm "$VM" --uartmode1 file "$BUILD/serial.log"
$VBM modifyvm "$VM" --usb off
$VBM modifyvm "$VM" --keyboard ps2
$VBM modifyvm "$VM" --mouse ps2
$VBM modifyvm "$VM" --audio-driver hda
$VBM modifyvm "$VM" --audiocontroller hda
$VBM modifyvm "$VM" --audioout on
$VBM modifyvm "$VM" --nestedpaging on

if [ ! -f "$BUILD/harddrive.vdi" ]; then
    echo "Convirtiendo el disco"
    $VBM convertfromraw "$IMG" "$BUILD/harddrive.vdi"
fi

echo "Adjuntando el disco"
$VBM storagectl "$VM" --name ATA --add sata --controller IntelAHCI --bootable on --portcount 1
$VBM storageattach "$VM" --storagectl ATA --port 0 --device 0 --type hdd --medium "$BUILD/harddrive.vdi"

echo "VM lista. Arráncala con: VBoxManage startvm $VM"
