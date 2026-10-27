#!/usr/bin/env bash
# Arranca la imagen 'wayland' en VirtualBox y captura el escritorio.
#
# El arranque es por BIOS y la consola de texto vive en VT1; el compositor
# orbital se levanta en VT3, asi que se espera a que aparezca y se captura
# la pantalla completa.
#
# Uso: probar-escritorio.sh [ruta/harddrive.img]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMG="${1:-$ROOT/build/x86_64/wayland/harddrive.img}"
VBM=VBoxManage
VM=Carcinization
OUT=/tmp/carcinization-escritorio

if [ ! -f "$IMG" ]; then
    echo "No se encuentra la imagen $IMG" >&2
    exit 1
fi
mkdir -p "$OUT"

echo "== Preparando la VM =="
"$ROOT/scripts/crear-vm.sh" "$IMG" || exit 1
# La GUI necesita mas memoria de video que la consola de texto.
$VBM modifyvm "$VM" --vram 64 >/dev/null 2>&1
$VBM modifyvm "$VM" --memory 4096 >/dev/null 2>&1
$VBM modifyvm "$VM" --uart1 0x3F8 4 --uartmode1 file "$OUT/serial.log" >/dev/null 2>&1

echo "== Arrancando =="
$VBM startvm "$VM" --type headless || exit 1
sleep 40

echo "== Pasando el menu de resolucion =="
$VBM controlvm "$VM" keyboardputscancode 1c 9c >/dev/null 2>&1

echo "== Esperando al compositor (VT3) =="
for i in $(seq 1 24); do
    sleep 15
    $VBM controlvm "$VM" screenshotpng "$OUT/paso-$i.png" >/dev/null 2>&1
    echo "  captura $i"
done

echo "== Capturas en $OUT =="
ls -la "$OUT"
