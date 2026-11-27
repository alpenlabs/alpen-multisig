-- Preserve opaque UTF-8 report text, including embedded NULs.
ALTER TABLE payout_admin_records ADD COLUMN report BYTEA;
