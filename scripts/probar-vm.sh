#!/usr/bin/env bash
# Arranca la VM de Carcinization OS en VirtualBox sin ventana y comprueba que
# el sistema arranca, entra en sesión y puede instalar un paquete de Debian.
#
# El puerto serie de la VM se conecta a una tubería con nombre, de modo que
# las teclas se pueden enviar con `printf` y la salida se puede leer.
#
# Uso: probar-vm.sh [ruta/harddrive.img]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IMG="${1:-$ROOT/build/x86_64/minimal/harddrive.img}"
VBM=VBoxManage
VM=Carcinization
PIPE=/tmp/carcinization-vm-serial
LOG=/tmp/carcinization-vm-serial.log

if [ ! -f "$IMG" ]; then
    echo "No se encuentra la imagen $IMG" >&2
    exit 1
fi

"$ROOT/scripts/crear-vm.sh" "$IMG" || exit 1

echo "== Conectando el puerto serie a $PIPE =="
rm -f "$PIPE" "$LOG"
$VBM modifyvm "$VM" --uart1 0x3F8 4 --uartmode1 pipe "$PIPE"

# La tubería tiene que existir antes de que la VM la abra.
mkfifo "$PIPE"
(while true; do cat "$PIPE"; done > "$LOG" 2>/dev/null &)

echo "== Arrancando la VM =="
$VBM startvm "$VM" --type headless || exit 1
sleep 30

echo "== Pasando el menú de resolución =="
printf '\r' > "$PIPE"
sleep 40

echo "== Entrando como root =="
printf 'root\n' > "$PIPE"
sleep 8
printf 'password\n' > "$PIPE"
sleep 15

echo "== Recogiendo el estado =="
printf 'uname -a; apt update; apt install hello; echo FIN_DE_LA_PRUEBA\n' > "$PIPE"
sleep 120

echo "== Apagando =="
printf 'poweroff\n' > "$PIPE"
sleep 20
$VBM controlvm "$VM" poweroff >/dev/null 2>&1

echo
echo "== Salida del guest =="
sed -e 's/\x1b\[[0-9;?]*[a-zA-Z]//g' "$LOG" | tail -60
