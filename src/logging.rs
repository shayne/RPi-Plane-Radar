use env_logger::Env;
use log::{Log, Metadata, Record, SetLoggerError};

struct LocationSafeLogger<L> {
    inner: L,
}

impl<L> LocationSafeLogger<L> {
    fn new(inner: L) -> Self {
        Self { inner }
    }
}

impl<L: Log> Log for LocationSafeLogger<L> {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        !is_ureq_target(metadata.target()) && self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if !is_ureq_target(record.target()) {
            self.inner.log(record);
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

fn is_ureq_target(target: &str) -> bool {
    target == "ureq"
        || target.starts_with("ureq::")
        || target == "ureq_proto"
        || target.starts_with("ureq_proto::")
}

pub fn init() -> Result<(), SetLoggerError> {
    let inner = env_logger::Builder::from_env(Env::default().default_filter_or("info")).build();
    let max_level = inner.filter();
    log::set_boxed_logger(Box::new(LocationSafeLogger::new(inner)))?;
    log::set_max_level(max_level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use log::{Level, Log, Metadata, Record};

    use super::LocationSafeLogger;

    struct PanicOnLog;

    impl Log for PanicOnLog {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn log(&self, _record: &Record<'_>) {
            panic!("inner logger received a filtered record");
        }

        fn flush(&self) {}
    }

    #[test]
    fn ureq_targets_stay_disabled_under_trace_without_reaching_the_inner_logger() {
        let logger = LocationSafeLogger::new(PanicOnLog);
        for target in ["ureq", "ureq::run", "ureq_proto", "ureq_proto::client"] {
            let metadata = Metadata::builder()
                .level(Level::Trace)
                .target(target)
                .build();
            assert!(!logger.enabled(&metadata));
            let record = Record::builder()
                .metadata(metadata)
                .args(format_args!(
                    "sensitive path 40.712800/lon/-74.006000/dist/7.2"
                ))
                .build();
            logger.log(&record);
        }

        let application = Metadata::builder()
            .level(Level::Trace)
            .target("planeradar::app")
            .build();
        assert!(logger.enabled(&application));
    }
}
