#!/usr/bin/env bash

set -e

MOUNT_POINT="/mnt/carcinizationfs"
DISK_DEVICE=""

show_help() {
    echo "Usage: $0 [options] <device>"
    echo ""
    echo "Mount or unmount a CarcinizationFS partition"
    echo ""
    echo "Options:"
    echo "  -u, --unmount    Unmount the CarcinizationFS partition"
    echo "  -m, --mount-point PATH    Custom mount point (default: /mnt/carcinizationfs)"
    echo "  -h, --help       Show this help"
    echo ""
    echo "Examples:"
    echo "  $0 /dev/sda3                    Mount /dev/sda3"
    echo "  $0 -u                           Unmount from default location"
    echo "  $0 -m /mnt/my-carcinization /dev/sda3   Mount to custom location"
}

unmount_fs() {
    if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "Unmounting CarcinizationFS from $MOUNT_POINT..."
        fusermount -u "$MOUNT_POINT" || fusermount3 -u "$MOUNT_POINT"
        echo "Successfully unmounted"
    else
        echo "Nothing mounted at $MOUNT_POINT"
    fi
    exit 0
}

check_dependencies() {
    # Try to find carcinizationfs in multiple locations
    CARCINIZATIONFS_BIN=""
    if [ -x "build/fstools/bin/redoxfs" ]; then
        CARCINIZATIONFS_BIN="build/fstools/bin/redoxfs"
    elif [ -x "$(dirname "$0")/../build/fstools/bin/redoxfs" ]; then
        CARCINIZATIONFS_BIN="$(dirname "$0")/../build/fstools/bin/redoxfs"
    elif command -v carcinizationfs &> /dev/null; then
        CARCINIZATIONFS_BIN="carcinizationfs"
    fi

    if [ -z "$CARCINIZATIONFS_BIN" ]; then
        echo "Error: carcinizationfs command not found"
        echo "Please build it first with: make fstools"
        exit 1
    fi

    if ! ldconfig -p 2>/dev/null | grep -q "libfuse3"; then
        echo "Error: libfuse 3.x is not installed"
        echo "Please install it:"
        if command -v apt-get &> /dev/null; then
            echo "  sudo apt-get install fuse3 libfuse3-dev"
        elif command -v dnf &> /dev/null; then
            echo "  sudo dnf install fuse3-devel"
        elif command -v pacman &> /dev/null; then
            echo "  sudo pacman -S fuse3"
        else
            echo "  (check your package manager for fuse3)"
        fi
        exit 1
    fi
}

UNMOUNT=false

while [[ $# -gt 0 ]]; do
    case $1 in
        -u|--unmount)
            UNMOUNT=true
            shift
            ;;
        -m|--mount-point)
            MOUNT_POINT="$2"
            shift 2
            ;;
        -h|--help)
            show_help
            exit 0
            ;;
        *)
            DISK_DEVICE="$1"
            shift
            ;;
    esac
done

if [ "$UNMOUNT" = true ]; then
    unmount_fs
fi

if [ -z "$DISK_DEVICE" ]; then
    DISK_DEVICE="/dev/disk/by-partlabel/REDOX_INSTALL"
    if [ ! -b "$DISK_DEVICE" ]; then
        echo "Error: No device specified and default partition not found"
        echo ""
        show_help
        exit 1
    fi
fi

if [ ! -b "$DISK_DEVICE" ] && [ ! -f "$DISK_DEVICE" ]; then
    echo "Error: $DISK_DEVICE is not a block device or file"
    exit 1
fi

check_dependencies

mkdir -p "$MOUNT_POINT"

echo "Mounting $DISK_DEVICE to $MOUNT_POINT..."
"$CARCINIZATIONFS_BIN" "$DISK_DEVICE" "$MOUNT_POINT"

echo "CarcinizationFS successfully mounted at $MOUNT_POINT"
echo "To unmount, run: $0 -u"

