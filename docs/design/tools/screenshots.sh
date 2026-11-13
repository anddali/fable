#!/usr/bin/env bash
# Render every mockup in light and dark with headless Chromium into
# docs/design/screens/<page>[-<variant>]-<theme>.png.
# Usage: docs/design/tools/screenshots.sh [page.html[?query] ...]
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
out="$here/screens"
mkdir -p "$out"
chrome="${CHROME:-$(command -v chromium || command -v chromium-browser || command -v google-chrome)}"

# page[?query]  output-name  height
default_pages=(
  "setup.html?step=1 setup-1-provider 1000"
  "setup.html?step=2 setup-2-roles 1000"
  "setup.html?step=3 setup-3-integration 1250"
  "setup.html?step=4 setup-4-collection 1000"
  "setup.html?step=5 setup-5-source 900"
  "setup.html?step=6 setup-6-search 1000"
  "admin-model-providers.html admin-model-providers 1000"
  "admin-model-roles.html admin-model-roles 1200"
  "admin-integrations.html admin-integrations 1000"
  "source-confluence.html source-confluence 1100"
  "collections.html collections 1100"
  "collection-detail.html collection-detail 1100"
  "indexing-settings.html indexing-settings 1500"
  "search.html search 1100"
  "chat.html chat 1000"
  "evaluation.html evaluation 1400"
)
if [ "$#" -gt 0 ]; then pages=("$@"); else pages=("${default_pages[@]}"); fi

for entry in "${pages[@]}"; do
  read -r url name height <<<"$entry"
  name="${name:-${url%%.html*}}"; height="${height:-1000}"
  sep="?"; [[ "$url" == *"?"* ]] && sep="&"
  for theme in light dark; do
    "$chrome" --headless=new --no-sandbox --disable-gpu --hide-scrollbars \
      --force-device-scale-factor=1 --window-size="1440,$height" \
      --virtual-time-budget=2000 \
      --screenshot="$out/$name-$theme.png" \
      "file://$here/$url${sep}theme=$theme" >/dev/null 2>&1
    echo "$out/$name-$theme.png"
  done
done
