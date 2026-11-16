-- Admin Block Payouts (Payout Administrators). Kept apart from the ASM proposal tables: these are
-- Bitcoin transactions signed by the admins in strata-bridge params.toml, not ASM actions.
CREATE TABLE IF NOT EXISTS payout_pending (
    txid TEXT PRIMARY KEY,
    psbt TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    first_signature_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ NOT NULL,
    broadcast_error TEXT,
    broadcast_error_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS payout_past (
    txid TEXT PRIMARY KEY,
    raw_tx TEXT NOT NULL,
    psbt TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    broadcast_at TIMESTAMPTZ NOT NULL
);
