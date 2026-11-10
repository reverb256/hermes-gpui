# hermes-hub chart

Deploys the single-owner hub: SQLite registry, node/device WebSockets, command
routing, audit and secret-prompt brokering. It is one replica by construction
because SQLite is a single-writer store. Scale nodes, not hub replicas.

## Before installing

Create the admin-token Secret (the chart references it and never creates it
unless `adminToken.create=true`, which is for local disposable tests only):

```bash
kubectl create namespace hermes
kubectl -n hermes create secret generic hermes-hub-admin \
  --from-literal=admin-token="$(openssl rand -base64 48)"
```

The token is the owner credential for enrollment, node listing and command
submission. Store it like an SSH private key; rotating it means updating the
Secret and restarting the Deployment.

## Install with Helm

```bash
helm upgrade --install hermes-hub charts/hermes-hub \
  --namespace hermes --create-namespace \
  --set image.tag=0.1.0 \
  --set tailscale.hostname=hermes
```

The default image is `ghcr.io/reverb256/hermes-hub`. Pin a digest in production:

```bash
--set image.digest=sha256:...
```

## ArgoCD

Apply [`deploy/argocd/hermes-hub.yaml`](../../deploy/argocd/hermes-hub.yaml).
The Application syncs `charts/hermes-hub` from `main` into `hermes` with
automated prune and self-heal. Create the admin Secret before the first sync.
Cluster-specific values belong in a separate overlay Application or a values
file that is not this chart's defaults.

## Tailscale

`tailscale.enabled=true` publishes the Service through the existing Tailscale
operator ingress class. No NodePort, LoadBalancer or inbound firewall rule is
needed. The hostname is a tailnet name; devices must be authorized by the
tailnet ACL and still need the hub bearer token.

To reach the cluster without the operator, set `tailscale.enabled=false` and
port-forward the Service. Do not expose this Service publicly: the admin bearer
token is the only application-level gate.

## Data

The SQLite database lives at `/data/hermes.db` on a PVC. The PVC carries
`helm.sh/resource-policy: keep`, so deleting the release does not delete command
history, audit or prompt metadata. Secrets are never stored there, only their
prompt metadata and the audit outcome. Unresolved prompts are closed on restart.

## Values

See [`values.yaml`](values.yaml). The important ones:

| Value | Default | Notes |
| --- | --- | --- |
| `adminToken.existingSecret` | `hermes-hub-admin` | required Secret name |
| `adminToken.key` | `admin-token` | key inside that Secret |
| `persistence.enabled` | `true` | SQLite PVC |
| `persistence.existingClaim` | `""` | use an existing claim instead |
| `tailscale.enabled` | `true` | Tailscale Ingress |
| `tailscale.hostname` | `hermes` | tailnet hostname |
| `hub.maxCommandTimeoutMs` | `900000` | upper bound accepted by the API |
| `resources` | small | one personal hub |

## Verify a release

```bash
helm lint charts/hermes-hub
helm template hermes-hub charts/hermes-hub --namespace hermes > /tmp/render.yaml
kubectl -n hermes rollout status deploy/hermes-hub
kubectl -n hermes port-forward svc/hermes-hub 8080:8080 &
curl -fsS http://127.0.0.1:8080/readyz
```
