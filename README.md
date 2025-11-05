
<p align="center">
  <img src="assets/world-chain.png" alt="World Chain">
</p>

# World Chain

World Chain is a blockchain designed for humans. Prioritizing scalability and accessibility for real users, World Chain provides the rails for a frictionless onchain UX. 

### Downloading Snapshots

reth snapshots are regularly updated and can be downloaded and extraced with the following commands:

```bash
BUCKET="world-chain-snapshots" # use world-chain-testnet-snapshots for sepolia
FILE_NAME="reth_archive.tar.lz4" # reth_full.tar.lz4 is available on mainnet only
OUT_DIR="./" # path to where you would like reth dir to end up
VID="$(aws s3api head-object --bucket "$BUCKET" --key "$FILE_NAME" --region eu-central-2 --query 'VersionId' --output text)"
aws s3api get-object --bucket "$BUCKET" --key "$FILE_NAME"  --version-id "$VID" --region eu-central-2 --no-cli-pager /dev/stdout | lz4 -d | tar -C "$OUT_DIR" -x
```

