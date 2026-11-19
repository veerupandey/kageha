#!/usr/bin/env bash
# Install only the optional Audio8 runtime; never changes Kageha's environment.
set -euo pipefail
runtime="${KAGEHA_HOME:-$HOME/.kageha}/runtimes/audio8"
if [[ ! -x "$runtime/bin/python" ]]; then
  uv venv "$runtime" --python 3.12
fi
uv pip install --python "$runtime/bin/python" \
  'torch>=2.5,<2.11' 'torchaudio>=2.5,<2.11' 'transformers==4.57.3' \
  'soundfile>=0.12' 'safetensors>=0.4'
"$runtime/bin/python" - <<'PY'
from huggingface_hub import snapshot_download
snapshot_download('Edge0/Audio8-TTS-Realistic-0.6b', revision='925bbd4c2d8360af85f2594bf0205173fda853ba',
                  allow_patterns=['*.py', '*.json', '*.safetensors', '*.pth', 'LICENSE', 'NOTICE'])
PY
printf 'Audio8 installed. Enable it in Settings → Media → Voice cloning.\n'
