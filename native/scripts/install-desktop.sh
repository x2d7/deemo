#!/bin/sh
# OPTIONAL REPAIR TOOL. The game registers .deemopack automatically on
# startup; run this only if the association was removed or blocked.
# Register Deemo as the handler for .deemopack files for the current user.
# Run from anywhere: scripts/install-desktop.sh
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
mime_dir="$data/mime/packages"
apps_dir="$data/applications"

# Absolute path to this tree's binary: packaged root first, then cargo output.
bin=${DEEMO_BIN:-}
if [ -z "$bin" ]; then
    for candidate in "$root/deemo" "$root/target/release/deemo" "$root/target/debug/deemo"; do
        if [ -x "$candidate" ]; then
            bin=$candidate
            break
        fi
    done
fi
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    echo "Deemo binary was not found. Build it first (cargo build --release)." >&2
    exit 1
fi
bin=$(CDPATH= cd -- "$(dirname -- "$bin")" && pwd)/$(basename -- "$bin")

mkdir -p "$mime_dir" "$apps_dir"

cat > "$mime_dir/deemo-deemopack.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/x-deemopack">
    <comment>Deemo pack</comment>
    <glob pattern="*.deemopack"/>
  </mime-type>
</mime-info>
EOF

# Desktop Exec quoting: paths with spaces go in double quotes, then %f.
exec_line=$bin
case "$bin" in
    *[[:space:]]*) exec_line="\"$bin\"" ;;
esac
cat > "$apps_dir/deemo.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Deemo
Comment=Deemo rhythm game
Exec=$exec_line %f
MimeType=application/x-deemopack;
Terminal=false
Categories=Game;
EOF

if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database "$data/mime" >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$apps_dir" >/dev/null 2>&1 || true
fi
if command -v xdg-mime >/dev/null 2>&1; then
    xdg-mime default deemo.desktop application/x-deemopack >/dev/null 2>&1 || true
fi

echo "Deemo binary: $bin"
echo "Undo: rm '$mime_dir/deemo-deemopack.xml' '$apps_dir/deemo.desktop'"
echo "      then run update-mime-database '$data/mime' and update-desktop-database '$apps_dir'"
