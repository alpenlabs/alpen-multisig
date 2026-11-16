use bitcoin::ScriptBuf;
pub fn first_unused_change(
    last_index: u32,
    mut script_at: impl FnMut(u32) -> Result<ScriptBuf, String>,
    is_used: impl Fn(u32) -> bool,
    taken: &std::collections::HashSet<ScriptBuf>,
) -> Result<Option<u32>, String> {
    for i in 0..=last_index {
        if !is_used(i) && !taken.contains(&script_at(i)?) {
            return Ok(Some(i));
        }
    }
    Ok(None)
}
