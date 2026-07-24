#!/usr/bin/env bash
set -euo pipefail

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_root=$(CDPATH= cd -- "${script_directory}/.." && pwd)
cd "${project_root}"

mapfile -d '' candidate_files < <(
  git ls-files --cached --others --exclude-standard -z
)

blocked_path=0
for path in "${candidate_files[@]}"; do
  case "${path}" in
    .env|*/.env|.env.*|*/.env.*|.envrc|*/.envrc)
      if [[ "${path}" != ".env.example" &&
            "${path}" != */.env.example ]]; then
        echo "error: refusing secret-like file name: ${path}" >&2
        blocked_path=1
      fi
      ;;
    *.pem|*.key|*.p12|*.pfx|*/id_rsa|*/id_ed25519)
      echo "error: refusing secret-like file name: ${path}" >&2
      blocked_path=1
      ;;
    credentials.json|*/credentials.json|secrets.json|*/secrets.json)
      echo "error: refusing secret-like file name: ${path}" >&2
      blocked_path=1
      ;;
  esac
done

if ((blocked_path != 0)); then
  exit 1
fi

# Keep the literal patterns in this script out of its own scan.
secret_pattern='-----BEGIN ([A-Z0-9]+ )?PRIVATE KEY-----'
secret_pattern+='|A(KIA|SIA)[0-9A-Z]{16}'
secret_pattern+='|gh[pousr]_[A-Za-z0-9_]{30,255}'
secret_pattern+='|github_pat_[A-Za-z0-9_]{30,255}'
secret_pattern+='|xox[baprs]-[A-Za-z0-9-]{20,}'
secret_pattern+='|AIza[0-9A-Za-z_-]{35}'
secret_pattern+='|sk-[A-Za-z0-9_-]{20,}'
secret_pattern+='|(postgres(ql)?|mysql|mongodb(\+srv)?|redis)://[^[:space:]/]+:[^[:space:]@]+@'
found_secret=0

for path in "${candidate_files[@]}"; do
  if [[ "${path}" == "scripts/check-secrets.sh" || ! -f "${path}" ]]; then
    continue
  fi

  if LC_ALL=C grep -EqI -- "${secret_pattern}" "${path}"; then
    echo "error: possible credential material found in ${path}" >&2
    found_secret=1
  fi
done

if ((found_secret != 0)); then
  exit 1
fi

echo "secret check passed"
