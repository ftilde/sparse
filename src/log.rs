use std::path::PathBuf;

#[allow(dead_code)]
pub enum Rotation {
    Never,
    Keep(usize),
}

pub fn init(rotation: Rotation) -> Result<(), Box<dyn std::error::Error>> {
    let cache_dir = dirs::cache_dir()
        .ok_or("Could not get cache dir")?
        .join(crate::APP_NAME);
    std::fs::create_dir_all(&cache_dir)?;

    if let Rotation::Keep(num) = rotation {
        clean_up(&cache_dir, num)?;
    }

    let dt = chrono::Local::now().naive_local();
    let log_file = dt
        .format(&format!("{}.log.%Y-%m-%d_%H:%M:%S", crate::APP_NAME))
        .to_string();
    let file_appender = tracing_appender::rolling::never(cache_dir, log_file);
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    Ok(())
}

fn clean_up(dir: &PathBuf, num: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|f| f.ok())
        .filter(|f| match f.path().file_name() {
            Some(f) => f
                .to_string_lossy()
                .starts_with(&format!("{}.log.", crate::APP_NAME)),
            None => false,
        })
        .map(|f| f.path())
        .collect::<Vec<_>>();
    // sort files so removing the first few will remove the oldest first
    files.sort();
    let remove: usize = if num <= files.len() {
        // remove one more because we will write a new file after cleanup
        files.len() - num + 1
    } else {
        0
    };
    files
        .iter()
        .take(remove)
        .map(|f| std::fs::remove_file(f))
        .fold(Ok(()), |acc, r| {
            if acc.is_err() || r.is_err() {
                return Err("Error deleting old log files");
            } else {
                Ok(())
            }
        })?;
    Ok(())
}
