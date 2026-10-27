#!/usr/bin/env bash
# Restaura los parches del cookbook a su version de upstream.
#
# El renombrado masivo de Redox a Carcinization cambio identificadores que
# los proyectos de terceros esperan con el nombre original ("redox"), asi que
# los parches ya no hacen lo que upstream espera.
#
# Solo se restaura un parche cuando la unica diferencia con upstream es ese
# renombrado: se deshace el renombrado sobre la copia local y se compara con
# upstream. Si hay otros cambios, se avisa y no se toca nada.
#
# Uso: restaurar-parches.sh [--apply]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BASE="https://gitlab.redox-os.org/redox-os/redox/-/raw/master/recipes"
APPLY=0
[ "${1:-}" = "--apply" ] && APPLY=1

cd "$ROOT" || exit 1
restaurados=0
con_dif=0

while IFS= read -r local; do
    dir="$(dirname "$local")"
    rel="${dir#recipes/}"
    name="$(basename "$local")"
    upname="${name//carcinization/redox}"
    tmp="/tmp/up-$(echo "$rel" | tr '/' '_')-$upname"

    if ! curl -sf "$BASE/$rel/$upname" -o "$tmp" 2>/dev/null || [ ! -s "$tmp" ]; then
        continue
    fi

    if diff -q "$tmp" "$local" >/dev/null 2>&1; then
        continue
    fi

    # Se deshace el renombrado sobre la copia local para comprobar que no hay
    # mas diferencias que el renombrado en si.
    rev="/tmp/rev-$(echo "$rel" | tr '/' '_')-$upname"
    sed -e 's/carcinization/redox/g' -e 's/Carcinization/Redox/g' -e 's/CARCINIZATION/REDOX/g' "$local" > "$rev"
    if diff -q "$tmp" "$rev" >/dev/null 2>&1; then
        echo "  -> $rel/$name : solo el renombrado, se restaura upstream"
        if [ "$APPLY" = "1" ]; then
            cp "$tmp" "$local" && restaurados=$((restaurados + 1))
        fi
    else
        n=$(diff "$tmp" "$local" | grep -c "^[<>]")
        echo "  !! $rel/$name : $n lineas distintas y no son solo el renombrado (NO se toca)"
        diff "$tmp" "$local" | head -6 | sed 's/^/       /'
        con_dif=$((con_dif + 1))
    fi
done < <(find recipes -name "*.patch" -not -path "*/source/*" -not -path "*/target/*" | sort)

echo "restaurados: $restaurados; con diferencias reales: $con_dif"
