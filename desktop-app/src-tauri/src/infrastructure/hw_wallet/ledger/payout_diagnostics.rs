use super::APDUCommand;

/// A status word, printed as the device documents it (`0x5515`).
pub(super) struct StatusHex(pub(super) u16);

impl std::fmt::Debug for StatusHex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:04x}", self.0)
    }
}

/// Diagnostics: with `LEDGER_APDU_TIMING=<file>`, appends one line per exchange with the device
/// (`end_ms cla ins duration_ms status`, no payload), to measure how long the device loads and signs.
pub(super) fn log_exchange_timing(cmd: &APDUCommand, started: std::time::Instant, retcode: u16) {
    use std::io::Write as _;
    let Ok(path) = std::env::var("LEDGER_APDU_TIMING") else {
        return;
    };
    let end_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            f,
            "{end_ms} {:02x} {:02x} {} {retcode:04x}",
            cmd.cla,
            cmd.ins,
            started.elapsed().as_millis()
        );
    }
}
