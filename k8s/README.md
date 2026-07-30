# Kubernetes deployment

These manifests run Drop as one full-stack Rust and Svelte container. The
`base` is portable Kubernetes, while `overlays/local` supports a local `kind`
cluster and `overlays/gke` adds Google Cloud-specific load balancing, TLS, and
connection draining.

## Why there is exactly one replica

Drop keeps active session metadata and transfer channels in one process.
Running multiple replicas without shared coordination can route the sender and
receiver to different pods. The Deployment therefore uses one replica and a
`Recreate` strategy.

This is an intentional first Kubernetes milestone, not a highly available
architecture. Before a production rollout, check `/metrics` and deploy when
`active_sessions` is zero. On termination, the server:

1. makes `/ready` return `503` and rejects new sessions with `503`;
2. waits `DROP_SHUTDOWN_DRAIN_DELAY_SECS` for load balancers to notice;
3. keeps the process alive until sessions and WebSockets finish, up to
   `DROP_SHUTDOWN_MAX_TRANSFER_WAIT_SECS`;
4. starts Axum's graceful server shutdown.

Both waits are environment variables rather than compile-time constants because
the honored shutdown budget differs per platform. The portable defaults are 10s
and 3500s; the GKE overlay sets 30s and 540s for the reasons in
[Autopilot shutdown budget](#autopilot-shutdown-budget).

A transfer that lasts beyond the grace period is still interrupted because
resume is not implemented. At the 4 GB maximum upload size, a sender slower than
roughly 60 Mbit/s cannot finish inside the 540s drain window on GKE, so drain
losses are expected for large transfers on slow links.

## Autopilot shutdown budget

Autopilot does not honor an arbitrary `terminationGracePeriodSeconds`. During
node auto-upgrades it caps most Pods at
[600 seconds](https://cloud.google.com/kubernetes-engine/docs/concepts/cluster-upgrades-autopilot)
and truncates anything longer without warning, so the portable base value of
3600 would not survive here. The GKE overlay therefore sets 600 and divides it:

| Setting | Value | Why |
| --- | --- | --- |
| `terminationGracePeriodSeconds` | 600s | The most Autopilot honors. |
| `DROP_SHUTDOWN_DRAIN_DELAY_SECS` | 30s | Exceeds the GCE health check detection window of `checkIntervalSec` 10 x `unhealthyThreshold` 2 = 20s. |
| `DROP_SHUTDOWN_MAX_TRANSFER_WAIT_SECS` | 540s | Leaves 30s inside the grace period for Axum's graceful shutdown. |

`scripts/check-gke-shutdown-budget.sh` runs in CI and enforces
`drain delay + transfer wait < grace period <= 600`, so this ordering cannot
regress unnoticed.

Two further Autopilot behaviours shape the overlay:

- **Eviction.** The Pod carries
  `cluster-autoscaler.kubernetes.io/safe-to-evict: "false"`, which makes it an
  [extended-duration Pod](https://cloud.google.com/kubernetes-engine/docs/how-to/extended-duration-pods)
  that runs at least seven days before a scale-down or node auto-upgrade may
  evict it. This does not protect against OOM kills, Compute Engine VM
  maintenance, preemption by higher-priority Pods, or a manual node drain. The
  PodDisruptionBudget covers voluntary drains for up to one hour.
- **Resource floors.** Autopilot enforces a minimum of 250m CPU and 512Mi memory
  per Pod and a CPU:memory ratio between 1:1 and 1:6.5. The base requests 256Mi,
  which Autopilot would silently raise, so the overlay requests 512Mi to keep the
  rendered manifest honest about what runs.

## WebSocket behaviour behind the GCP load balancer

The external Application Load Balancer supports WebSockets without extra
configuration, but the `timeoutSec: 60` in `backend-config.yaml` is easy to
misread. Per
[Google's backend service timeout documentation](https://cloud.google.com/load-balancing/docs/https/request-distribution#timeout-bes),
an **active** WebSocket does not use that timeout and is instead closed after 24
hours; the timeout applies to **idle** connections. The server's 15s heartbeat
keeps a transferring connection well inside the 60s idle budget, so `timeoutSec`
does not cap transfer duration.

Because a hard container memory limit now applies, the relay also caps inbound
WebSocket messages at `WS_MAX_MESSAGE_BYTES` (256 KiB). Axum's default of 64 MiB
would let a hostile sender queue
`MAX_CONCURRENT_SESSIONS * DOWNLOAD_EVENT_CHANNEL_CAPACITY` oversized chunks and
push the pod past its 512Mi limit into an OOM kill. The browser client sends
64 KiB chunks, so the cap leaves four times the headroom it needs.

## Run locally with kind

Install Docker, `kubectl`, and
[`kind`](https://kind.sigs.k8s.io/docs/user/quick-start/), then run:

```bash
docker build -f Dockerfile.fullstack -t drop:dev .
kind create cluster --name drop
kind load docker-image drop:dev --name drop
kubectl apply -k k8s/overlays/local
kubectl wait --namespace drop \
  --for=condition=available deployment/drop \
  --timeout=120s
kubectl port-forward --namespace drop service/drop 8080:80
```

Open `http://127.0.0.1:8080`. In another terminal, inspect the objects and
application logs:

```bash
kubectl get all --namespace drop
kubectl describe deployment drop --namespace drop
kubectl logs deployment/drop --namespace drop --follow
```

Delete the learning cluster when finished:

```bash
kind delete cluster --name drop
```

## Troubleshooting the local cluster

Both failures below are host networking or permissions, not Kubernetes, and
neither error message points anywhere near its real cause.

### `kubectl` reports `EOF` and nothing works

```text
error validating data: failed to download openapi:
Get "https://127.0.0.1:44451/openapi/v2?timeout=32s": EOF
```

The symptom is that the API server port accepts a TCP connection and then drops
it immediately. That happens when `docker-proxy` binds the host port
successfully but cannot reach the container behind it, so the cluster looks
broken while actually being healthy.

Confirm the cluster is fine before touching it:

```bash
# API server inside the node container: expect HTTP 200
docker exec drop-control-plane \
  curl -sk -o /dev/null -w '%{http_code}\n' https://localhost:6443/healthz

# Another container on the same network: expect HTTP 200
docker run --rm --network kind curlimages/curl -sk -o /dev/null \
  -w '%{http_code}\n' https://172.18.0.2:6443/healthz

# The host itself: this is the hop that fails
curl -sk -o /dev/null -w '%{http_code}\n' https://172.18.0.2:6443/healthz
```

If only the last one fails, nothing is wrong with Kubernetes. The usual cause is
a **VPN kill switch** rejecting traffic that does not leave through the tunnel.
Mullvad's lockdown mode installs an `inet mullvad` nftables table whose output
chain ends in a bare `reject`, so packets to the Docker bridge are refused. The
giveaway is an instant `Connection refused` plus this from `ping`:

```text
From 172.18.0.1 icmp_seq=1 Destination Port Unreachable
```

Docker networks live inside `172.16.0.0/12`, so allowing local network sharing
fixes it:

```bash
mullvad lan get          # "block" means this is your problem
mullvad lan set allow
```

Other VPN clients have an equivalent "allow LAN / local network" setting. Note
that container-to-container traffic keeps working throughout, because same
bridge traffic is switched at layer 2 and never reaches these rules — which is
what makes the failure so confusing.

Check for a real firewall only after ruling the VPN out, and check whether it is
actually *enabled* rather than merely installed:

```bash
grep ENABLED /etc/ufw/ufw.conf   # ENABLED=no means ufw is not your problem
```

`systemctl is-active ufw` reports the service unit, not whether the firewall is
enforcing anything, so it is misleading here.

### `permission denied` on `/var/run/docker.sock`

If `id -nG` does not list `docker` but `getent group docker` shows your user,
the membership exists on disk but your login session predates it. Group
membership is resolved at login, so re-running `usermod -aG docker $USER`
changes nothing.

```bash
newgrp docker    # fixes the current shell only
```

Log out and back in for a durable fix. Inside an IDE, restart the IDE itself:
its terminals inherit the IDE process's credentials, so new terminals stay stale
until it restarts.

## Deploy to GKE Autopilot

The example region is `europe-north1`. Select the region closest to the users
who exchange files because every byte passes through the Drop pod.

Set shell variables without putting credentials in the repository:

```bash
export DROP_PROJECT_ID="your-gcp-project-id"
export DROP_REGION="europe-north1"
export DROP_IMAGE="${DROP_REGION}-docker.pkg.dev/${DROP_PROJECT_ID}/drop/drop:latest"

gcloud config set project "$DROP_PROJECT_ID"
gcloud services enable \
  artifactregistry.googleapis.com \
  compute.googleapis.com \
  container.googleapis.com
```

Create an Artifact Registry repository, an Autopilot cluster, and a stable
global IP:

```bash
gcloud artifacts repositories create drop \
  --repository-format=docker \
  --location="$DROP_REGION"
gcloud auth configure-docker "${DROP_REGION}-docker.pkg.dev"
gcloud container clusters create-auto drop \
  --region="$DROP_REGION"
gcloud container clusters get-credentials drop \
  --region="$DROP_REGION"
gcloud compute addresses create drop-ip --global
```

Build and push the full-stack image:

```bash
docker build -f Dockerfile.fullstack -t "$DROP_IMAGE" .
docker push "$DROP_IMAGE"
```

Before applying the overlay, replace both placeholders:

- replace `replace-project-id` in
  `k8s/overlays/gke/kustomization.yaml` with the GCP project ID;
- replace `drop.example.com` in both `managed-certificate.yaml` and
  `ingress.yaml` with a staging domain you control.

Apply and inspect the deployment:

```bash
kubectl apply -k k8s/overlays/gke
kubectl rollout status deployment/drop \
  --namespace drop \
  --timeout=10m
kubectl get ingress,managedcertificate \
  --namespace drop
gcloud compute addresses describe drop-ip \
  --global \
  --format='value(address)'
```

Point the staging domain's `A` record to the printed static address. Google
must see that DNS record before it can provision the managed certificate. Load
balancer and certificate provisioning can take several minutes.

The GKE overlay trusts the GCP Application Load Balancer's appended
`X-Forwarded-For` values so per-IP limits use the original client address. Do
not set `DROP_TRUST_GCP_X_FORWARDED_FOR=true` when the pod is directly exposed
to untrusted traffic or placed behind a different proxy format.

## Updating the deployment

Push an immutable image tag, update `newTag` in the GKE kustomization, verify
that there are no active sessions, then apply:

```bash
curl --fail https://drop.example.com/metrics
kubectl apply -k k8s/overlays/gke
kubectl rollout status deployment/drop \
  --namespace drop \
  --timeout=15m
```

The `Recreate` strategy means the old pod fully stops before the new one starts,
so a deploy during an active transfer blocks for up to the 600s grace period and
the site is down for that time. Deploying when `active_sessions` is zero keeps
the changeover to the length of an image pull and a startup probe.

Avoid reusing `latest` for real releases because Kubernetes and registries can
make it hard to tell which build is running.

## Cost cleanup

GKE compute, the external Application Load Balancer, its public IPv4 address,
Artifact Registry, and network egress can all incur charges. When the
environment is only for practice, remove it:

```bash
kubectl delete -k k8s/overlays/gke
gcloud container clusters delete drop \
  --region="$DROP_REGION"
gcloud compute addresses delete drop-ip --global
gcloud artifacts repositories delete drop \
  --location="$DROP_REGION"
```
