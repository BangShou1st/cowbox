#!/usr/bin/env bash
set -euo pipefail

exec "${COWBOX:-cowbox}" demo "$@"
