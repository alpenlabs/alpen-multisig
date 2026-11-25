-- Separate from ASM proposals. Each record is pending or past; transition is one CAS write.
CREATE TABLE payout_admin_records (
    txid TEXT PRIMARY KEY,
    version BIGINT NOT NULL,
    body TEXT NOT NULL
);
