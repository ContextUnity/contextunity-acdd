#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

cleanup() {
  rm -rf content/docs
}
trap cleanup EXIT

python3 sync-docs.py

case "${1:-build}" in
  serve|--serve)
    shift || true
    zola serve "$@"
    ;;
  check|--check)
    shift || true
    zola build "$@"
    zola check --skip-external-links
    ;;
  build|--build)
    if [[ "${1:-}" == "build" || "${1:-}" == "--build" ]]; then
      shift || true
    fi
    zola build "$@"
    ;;
  *)
    zola build "$@"
    ;;
esac
