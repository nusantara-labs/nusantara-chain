#!/bin/bash
set -euo pipefail

LEDGER_PATH="${LEDGER_PATH:-/data/ledger}"
GENESIS_LEDGER="${GENESIS_LEDGER:-/genesis/ledger}"

# Init-only mode: generate keypairs for all validators, apply genesis, exit
if [ "${INIT_ONLY:-false}" = "true" ]; then
    # Generate keypairs for each validator into /genesis/
    # The genesis builder binds "generate" identities to these in order
    for i in 1 2 3; do
        KEYFILE="/genesis/validator${i}.key"
        if [ ! -f "$KEYFILE" ]; then
            nusantara-validator --generate-keypair "$KEYFILE"
            echo "[init] Generated keypair: validator${i}.key"
        fi
    done

    exec nusantara-validator --ledger-path "$LEDGER_PATH" \
        --genesis-config /etc/nusantara/genesis.toml --init-only \
        --identity=/genesis/validator1.key \
        --extra-validator-keys "/genesis/validator2.key,/genesis/validator3.key"
fi

# Wait for shared genesis ledger if our ledger doesn't exist yet
if [ ! -f "$LEDGER_PATH/CURRENT" ]; then
    for i in $(seq 1 60); do
        [ -f "$GENESIS_LEDGER/CURRENT" ] && break
        echo "[entrypoint] Waiting for genesis ledger... ($i/60)"
        sleep 1
    done

    if [ ! -f "$GENESIS_LEDGER/CURRENT" ]; then
        echo "ERROR: genesis ledger not ready after 60s"
        exit 1
    fi

    # Brief delay to ensure genesis-init has fully flushed
    sleep 2

    # Copy shared genesis ledger to our local data dir
    mkdir -p "$LEDGER_PATH"
    cp -a "$GENESIS_LEDGER"/* "$LEDGER_PATH"/
fi

# Determine which keypair this validator uses
VALIDATOR_NUM="${VALIDATOR_NUM:-}"
if [ -n "$VALIDATOR_NUM" ] && [ -f "/genesis/validator${VALIDATOR_NUM}.key" ]; then
    IDENTITY_FLAG="--identity=/genesis/validator${VALIDATOR_NUM}.key"
    echo "[entrypoint] Using pre-generated keypair: validator${VALIDATOR_NUM}.key"
elif [ "${IS_BOOTSTRAP:-false}" = "true" ] && [ -f "/genesis/validator1.key" ]; then
    IDENTITY_FLAG="--identity=/genesis/validator1.key"
    echo "[entrypoint] Bootstrap node using validator1.key"
else
    # Non-bootstrap without explicit assignment: generate own keypair
    IDENTITY_FLAG=""
    echo "[entrypoint] Using auto-generated keypair"
fi

exec nusantara-validator $IDENTITY_FLAG "$@"
