//! Validate captured scheduler identities without assuming the object table's
//! namespace offsets, generation counters or application launch order.
pub fn launched(serial: &str, name: &str) -> (u64, u32)
{
    let (app, thread) = serial.lines()
                              .rev()
                              .find_map(|line| {
                                  let line = line.strip_prefix("PROCESS_LAUNCHED app=")?;
                                  let (app, line) = line.split_once(" thread=")?;
                                  let (thread, actual_name) = line.split_once(" name=")?;
                                  if actual_name != name
                                  {
                                      return None;
                                  }
                                  Some((app.parse::<u64>().ok()?, thread.parse::<u32>().ok()?))
                              })
                              .unwrap_or_else(|| panic!("{name} launch missing: {serial}"));
    assert!(app > 1 && thread >> 16 != 0 && thread & 0xffff != 0,
            "invalid application/thread identity: app={app} thread={thread}");
    let prefix = format!("PROCESS_THREAD app={app} thread={thread} stack=");
    assert!(serial.lines().any(|line| line.starts_with(&prefix)),
            "launched thread lacks its actual scheduler record: {serial}");
    (app, thread)
}
