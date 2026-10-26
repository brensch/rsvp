#!/usr/bin/env bash
# One-time GCP infrastructure for the RSVP portal. Run from your laptop with gcloud authenticated.
#
#   PROJECT=my-project DOMAIN=wedding.example.com ./deploy/gcp-setup.sh
#
# Creates:
#   - a dedicated service account (only allowed to write the two buckets)
#   - gs://$PROJECT-rsvp-litestream : continuous SQLite replication (versioned, soft-delete)
#   - gs://$PROJECT-rsvp-snapshots  : daily snapshots, retention-locked so nothing can delete them early
#   - a static IP, firewall (80/443 public; SSH only via Identity-Aware Proxy)
#   - an e2-small Ubuntu 24.04 VM with Shielded VM + daily disk snapshots
set -euo pipefail

: "${PROJECT:?set PROJECT}"
: "${DOMAIN:?set DOMAIN}"
REGION="${REGION:-australia-southeast1}"
ZONE="${ZONE:-${REGION}-a}"
VM="${VM:-rsvp}"
MACHINE="${MACHINE:-e2-small}"       # e2-micro works with swap, but Claude Code is happier with 2 GB
DISK_TYPE="${DISK_TYPE:-pd-balanced}" # free tier: e2-micro + pd-standard (≤30 GB) in us-west1/us-central1/us-east1
DISK_SIZE="${DISK_SIZE:-20GB}"
SNAPSHOTS="${SNAPSHOTS:-1}"          # daily boot-disk snapshots (small extra cost); 0 to skip
SA="rsvp-vm"
SA_EMAIL="${SA}@${PROJECT}.iam.gserviceaccount.com"
LS_BUCKET="${PROJECT}-rsvp-litestream"
SNAP_BUCKET="${PROJECT}-rsvp-snapshots"
RETENTION="${RETENTION:-400d}"       # snapshots can't be deleted for this long

g() { echo "+ gcloud $*" >&2; gcloud --project "$PROJECT" "$@"; }

g services enable compute.googleapis.com storage.googleapis.com iap.googleapis.com

# Service account with no project-level roles; bucket-level grants only.
g iam service-accounts describe "$SA_EMAIL" >/dev/null 2>&1 || \
  g iam service-accounts create "$SA" --display-name "RSVP VM"

for B in "$LS_BUCKET" "$SNAP_BUCKET"; do
  g storage buckets describe "gs://$B" >/dev/null 2>&1 || \
    g storage buckets create "gs://$B" --location "$REGION" --uniform-bucket-level-access \
      --public-access-prevention --soft-delete-duration 30d
done
g storage buckets update "gs://$LS_BUCKET" --versioning
# Litestream must create/delete its own WAL segments: objectAdmin on its bucket only.
g storage buckets add-iam-policy-binding "gs://$LS_BUCKET" --member "serviceAccount:$SA_EMAIL" --role roles/storage.objectAdmin
# Snapshots: write-only (create), cannot delete or overwrite; plus a retention policy.
g storage buckets update "gs://$SNAP_BUCKET" --retention-period "$RETENTION"
g storage buckets add-iam-policy-binding "gs://$SNAP_BUCKET" --member "serviceAccount:$SA_EMAIL" --role roles/storage.objectCreator
echo ">>> To make the snapshot retention permanent (irreversible!), run:"
echo "    gcloud storage buckets update gs://$SNAP_BUCKET --lock-retention-period --project $PROJECT"

# Network
g compute addresses describe "$VM-ip" --region "$REGION" >/dev/null 2>&1 || \
  g compute addresses create "$VM-ip" --region "$REGION"
IP=$(gcloud --project "$PROJECT" compute addresses describe "$VM-ip" --region "$REGION" --format='value(address)')

g compute firewall-rules describe rsvp-web >/dev/null 2>&1 || \
  g compute firewall-rules create rsvp-web --allow tcp:80,tcp:443 --target-tags rsvp --source-ranges 0.0.0.0/0
g compute firewall-rules describe rsvp-iap-ssh >/dev/null 2>&1 || \
  g compute firewall-rules create rsvp-iap-ssh --allow tcp:22 --target-tags rsvp --source-ranges 35.235.240.0/20

# Daily disk snapshots of the boot disk (covers the journal and everything else on disk).
[ "$SNAPSHOTS" = "1" ] && { g compute resource-policies describe rsvp-daily --region "$REGION" >/dev/null 2>&1 || \
  g compute resource-policies create snapshot-schedule rsvp-daily --region "$REGION" \
    --daily-schedule --start-time 16:00 --max-retention-days 21 --on-source-disk-delete keep-auto-snapshots; }

if ! g compute instances describe "$VM" --zone "$ZONE" >/dev/null 2>&1; then
  g compute instances create "$VM" --zone "$ZONE" --machine-type "$MACHINE" \
    --image-family ubuntu-2404-lts-amd64 --image-project ubuntu-os-cloud \
    --boot-disk-size "$DISK_SIZE" --boot-disk-type "$DISK_TYPE" \
    --address "$IP" --tags rsvp \
    --service-account "$SA_EMAIL" --scopes https://www.googleapis.com/auth/devstorage.read_write \
    --shielded-secure-boot --shielded-vtpm --shielded-integrity-monitoring \
    --metadata enable-oslogin=TRUE,block-project-ssh-keys=TRUE
  [ "$SNAPSHOTS" = "1" ] && g compute disks add-resource-policies "$VM" --zone "$ZONE" --resource-policies rsvp-daily
fi

cat <<EOF

Done. Next:
  1. Point DNS:  $DOMAIN  A  $IP
  2. Copy config/wedding.example.toml -> deploy/wedding.toml and edit it
     (base_url https://$DOMAIN, rp_id $DOMAIN, backup.bucket = "$SNAP_BUCKET").
  3. Provision:  ZONE=$ZONE VM=$VM PROJECT=$PROJECT DOMAIN=$DOMAIN LS_BUCKET=$LS_BUCKET ./deploy/provision.sh
  4. Deploy:     ZONE=$ZONE VM=$VM PROJECT=$PROJECT ./deploy/deploy.sh
EOF
