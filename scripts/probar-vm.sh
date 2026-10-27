#!/usr/bin/env bash
# Arranca la VM de Carcinization OS en VirtualBox sin ventana y comprueba que
# el sistema arranca, entra en sesión y responde.
#
# El arranque con BIOS no escribe nada por el puerto serie, así que la
# consola se maneja con el teclado emulado y se lee con capturas de la
# pantalla.
#
# Uso: probar-vm.sh [ruta/harddrive.img]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMG="${1:-$ROOT/build/x86_64/minimal/harddrive.img}"
VBM=VBoxManage
VM=Carcinization
SHOTS="${SHOTS:-/tmp/carcinization-vm}"

if [ ! -f "$IMG" ]; then
    echo "No se encuentra la imagen $IMG" >&2
    exit 1
fi

mkdir -p "$SHOTS"

"$ROOT/scripts/crear-vm.sh" "$IMG" || exit 1

# Envía una línea de texto al guest y la confirma con Enter.
send() {
    $VBM controlvm "$VM" keyboardputstring "$1" >/dev/null 2>&1
    sleep 1
    $VBM controlvm "$VM" keyboardputscancode 1c 9c >/dev/null 2>&1
}

shot() {
    $VBM controlvm "$VM" screenshotpng "$SHOTS/$1.png" >/dev/null 2>&1
    echo "captura: $SHOTS/$1.png"
}

echo "== Arrancando la VM =="
$VBM startvm "$VM" --type headless || exit 1
sleep 35

echo "== Pasando el menú de resolución =="
$VBM controlvm "$VM" keyboardputscancode 1c 9c >/dev/null 2>&1
sleep 40

echo "== Entrando como root =="
send "root"
sleep 3
send "password"
sleep 12
shot "login"

echo "== Comprobando el sistema =="
send "uname -a"
sleep 6
send "ifconfig"
sleep 8
shot "sistema"

echo "== Probando la red =="
send "ping -c 2 10.0.2.2"
sleep 15
shot "red"

echo "== Apagando =="
send "poweroff"
sleep 20
$VBM controlvm "$VM" poweroff >/dev/null 2>&1
echo "Hecho. Las capturas están en $SHOTS"
