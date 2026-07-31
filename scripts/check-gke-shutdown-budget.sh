#!/usr/bin/env bash
set -euo pipefail

# GKE Autopilot honors at most 600 seconds of terminationGracePeriodSeconds
# during node auto-upgrades. Anything longer is silently truncated, which would
# cut off in-flight transfers instead of draining them. This guards the ordering
# the GKE overlay depends on:
#
#   drain delay + max transfer wait < terminationGracePeriodSeconds <= 600
#
# so the pod always finishes its own shutdown before Kubernetes sends SIGKILL.

readonly autopilot_grace_ceiling=600

rendered_manifest=${1:-}
if [[ -z "${rendered_manifest}" || ! -f "${rendered_manifest}" ]]; then
  echo "usage: $0 <rendered-gke-manifest.yaml>" >&2
  exit 2
fi

read_value() {
  local pattern=$1
  grep -oE "${pattern}" "${rendered_manifest}" | head -1 | grep -oE '[0-9]+' || true
}

grace_period=$(read_value 'terminationGracePeriodSeconds: [0-9]+')
drain_delay=$(grep -A1 'name: DROP_SHUTDOWN_DRAIN_DELAY_SECS' "${rendered_manifest}" |
  grep -oE '[0-9]+' | head -1 || true)
transfer_wait=$(grep -A1 'name: DROP_SHUTDOWN_MAX_TRANSFER_WAIT_SECS' "${rendered_manifest}" |
  grep -oE '[0-9]+' | head -1 || true)

for name in grace_period drain_delay transfer_wait; do
  if [[ -z "${!name}" ]]; then
    echo "error: could not read ${name} from ${rendered_manifest}" >&2
    exit 1
  fi
done

failed=0

if ((grace_period > autopilot_grace_ceiling)); then
  echo "error: terminationGracePeriodSeconds ${grace_period} exceeds the" \
    "Autopilot ceiling of ${autopilot_grace_ceiling}s and would be truncated" >&2
  failed=1
fi

shutdown_budget=$((drain_delay + transfer_wait))
if ((shutdown_budget >= grace_period)); then
  echo "error: drain delay (${drain_delay}s) plus max transfer wait" \
    "(${transfer_wait}s) is ${shutdown_budget}s, which leaves no headroom" \
    "inside terminationGracePeriodSeconds (${grace_period}s)" >&2
  failed=1
fi

# The GCE health check needs checkIntervalSec * unhealthyThreshold to stop
# sending new connections. Draining sooner than that sends new senders to a pod
# that is already refusing them.
check_interval=$(read_value 'checkIntervalSec: [0-9]+')
unhealthy_threshold=$(read_value 'unhealthyThreshold: [0-9]+')

if [[ -n "${check_interval}" && -n "${unhealthy_threshold}" ]]; then
  detection_seconds=$((check_interval * unhealthy_threshold))
  if ((drain_delay < detection_seconds)); then
    echo "error: drain delay (${drain_delay}s) is shorter than the GCE health" \
      "check detection window (${check_interval}s x ${unhealthy_threshold} =" \
      "${detection_seconds}s)" >&2
    failed=1
  fi
fi

if ((failed != 0)); then
  exit 1
fi

echo "GKE shutdown budget check passed:" \
  "${drain_delay}s drain + ${transfer_wait}s transfer wait" \
  "< ${grace_period}s grace period <= ${autopilot_grace_ceiling}s Autopilot ceiling"
