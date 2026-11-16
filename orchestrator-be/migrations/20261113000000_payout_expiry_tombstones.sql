-- Expiry is terminal for a txid: replaying an old PSBT must not restart its signing clock.
CREATE TABLE payout_expired (
    txid TEXT PRIMARY KEY
);
